//! Bounded two-bank journal for SPD and chipset-owned training payloads.
//!
//! Digests detect torn writes/corruption, not malicious flash replacement.
//! Payload validity and topology/policy identity belong to the memory driver.
//! Write only from a RAM-resident stage, after successful cold memory testing.

use fstart_core::services::ServiceError;
use fstart_driver_intel::BootPath;
use sha2::{Digest, Sha256};
use zerocopy::byteorder::{LittleEndian, U32};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

/// Platform-owned CAR buffers: the cached record and the new training output.
/// They are independent so replay input cannot alias the driver's capture.
pub struct TrainingBuffers<'a> {
    pub cached: &'a mut [u8],
    pub capture: &'a mut [u8],
}

/// One-shot static storage, instantiated at the platform's chosen capacity.
/// The common journal and boot flow only see the borrowed slices.
pub struct TrainingWorkspace<const N: usize> {
    buffers: core::cell::UnsafeCell<[[u8; N]; 2]>,
    taken: core::sync::atomic::AtomicBool,
}

// SAFETY: only the successful take() can access the private buffers; the
// token is never released, so no second mutable reference can be created.
unsafe impl<const N: usize> Sync for TrainingWorkspace<N> {}

impl<const N: usize> TrainingWorkspace<N> {
    pub const fn new() -> Self {
        assert!(N > 0, "training buffers must not be empty");
        Self {
            buffers: core::cell::UnsafeCell::new([[0; N]; 2]),
            taken: core::sync::atomic::AtomicBool::new(false),
        }
    }

    pub fn take(&'static self) -> Option<TrainingBuffers<'static>> {
        use core::sync::atomic::Ordering;
        self.taken
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()?;
        // SAFETY: this call owns the only token, and the storage is static.
        let [cached, capture] = unsafe { &mut *self.buffers.get() };
        Some(TrainingBuffers { cached, capture })
    }
}

impl<const N: usize> Default for TrainingWorkspace<N> {
    fn default() -> Self {
        Self::new()
    }
}
const MAGIC: [u8; 8] = *b"FSTMRC01";

#[derive(Clone, Copy, PartialEq, Eq, FromBytes, IntoBytes, Immutable, KnownLayout)]
#[repr(C)]
struct Header {
    magic: [u8; 8],
    version: U32<LittleEndian>,
    sequence: U32<LittleEndian>,
    length: U32<LittleEndian>,
    reserved: U32<LittleEndian>,
    key: [u8; 32],
    digest: [u8; 32],
    commit: U32<LittleEndian>,
}
const HEADER_SIZE: usize = core::mem::size_of::<Header>();
const COMMIT_OFFSET: usize = HEADER_SIZE - 4;
const DIGEST_OFFSET: usize = COMMIT_OFFSET - 32;

impl Header {
    fn payload_len(&self, extent: usize, committed: bool) -> Option<usize> {
        let length = self.length.get() as usize;
        (self.magic == MAGIC
            && self.version.get() == 1
            && self.reserved.get() == 0
            && length != 0
            && HEADER_SIZE
                .checked_add(length)
                .is_some_and(|end| end <= extent)
            && self.commit.get() == if committed { 0 } else { u32::MAX })
        .then_some(length)
    }
}

/// Narrow storage contract; every offset is relative to the trusted cache
/// extent. The backend enforces physical bounds, chip geometry and protection.
pub trait Storage {
    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), ServiceError>;
    fn program(&mut self, offset: u32, bytes: &[u8]) -> Result<(), ServiceError>;
    fn erase(&mut self, offset: u32, length: u32) -> Result<(), ServiceError>;
}

/// A validated payload view; moving a record never copies the training bytes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Record<'a> {
    pub key: [u8; 32],
    pub sequence: u32,
    payload: &'a [u8],
}
impl<'a> Record<'a> {
    pub fn new(key: [u8; 32], payload: &'a [u8]) -> Result<Self, ServiceError> {
        if payload.is_empty()
            || u32::try_from(payload.len()).is_err()
            || HEADER_SIZE.checked_add(payload.len()).is_none()
        {
            return Err(ServiceError::InvalidParam);
        }
        Ok(Self {
            key,
            sequence: 0,
            payload,
        })
    }
    pub fn payload(&self) -> &'a [u8] {
        self.payload
    }
    /// Exact encoded size for a pending record in the firmware store.
    pub fn encoded_size(&self) -> usize {
        HEADER_SIZE + self.payload.len()
    }

    /// Validate a committed record in caller-owned RAM without copying it.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, ServiceError> {
        let header = bytes
            .get(..HEADER_SIZE)
            .and_then(|bytes| Header::ref_from_bytes(bytes).ok())
            .ok_or(ServiceError::HardwareError)?;
        let length = header
            .payload_len(bytes.len(), true)
            .ok_or(ServiceError::HardwareError)?;
        let payload = &bytes[HEADER_SIZE..HEADER_SIZE + length];
        if checksum(&bytes[..DIGEST_OFFSET], payload) != header.digest {
            return Err(ServiceError::HardwareError);
        }
        Ok(Self {
            key: header.key,
            sequence: header.sequence.get(),
            payload,
        })
    }
}

/// Geometry for exactly two independently erasable banks.
#[derive(Clone, Copy)]
pub struct Journal {
    bank_size: u32,
}
impl Journal {
    pub fn new(bank_size: u32, extent_size: u32) -> Result<Self, ServiceError> {
        if !bank_size.is_power_of_two()
            || bank_size < 4096
            || bank_size.checked_mul(2) != Some(extent_size)
        {
            return Err(ServiceError::InvalidParam);
        }
        Ok(Self { bank_size })
    }

    // Verification uses a fixed chunk, never a payload-sized stack object.
    fn read_bank(
        &self,
        flash: &mut impl Storage,
        bank: usize,
        committed: bool,
        capacity: usize,
    ) -> Result<Option<Header>, ServiceError> {
        let offset = bank as u32 * self.bank_size;
        let mut bytes = [0u8; HEADER_SIZE];
        flash.read(offset, &mut bytes)?;
        let header = Header::read_from_bytes(&bytes).map_err(|_| ServiceError::InvalidParam)?;
        let Some(length) = header
            .payload_len(self.bank_size as usize, committed)
            .filter(|length| *length <= capacity)
        else {
            return Ok(None);
        };
        let mut hash = Sha256::new();
        hash.update(&bytes[..DIGEST_OFFSET]);
        let mut chunk = [0; 64];
        for at in (0..length).step_by(chunk.len()) {
            let count = chunk.len().min(length - at);
            flash.read(offset + HEADER_SIZE as u32 + at as u32, &mut chunk[..count])?;
            hash.update(&chunk[..count]);
        }
        let digest: [u8; 32] = hash.finalize().into();
        Ok((digest == header.digest).then_some(header))
    }

    fn valid_banks(
        &self,
        flash: &mut impl Storage,
        capacity: usize,
    ) -> Result<[Option<(usize, Header)>; 2], ServiceError> {
        let mut banks = [
            self.read_bank(flash, 0, true, capacity)?
                .map(|header| (0, header)),
            self.read_bank(flash, 1, true, capacity)?
                .map(|header| (1, header)),
        ];
        if matches!((&banks[0], &banks[1]), (Some((_, a)), Some((_, b)))
            if newer(b.sequence.get(), a.sequence.get()))
        {
            banks.swap(0, 1);
        }
        Ok(banks)
    }

    /// Load into caller-owned storage and borrow the verified payload from it.
    /// Oversized records are cache misses; I/O failures remain errors. Callers
    /// must match `key` and decode the payload. An S3 miss must reset, not train.
    pub fn load<'a>(
        &self,
        flash: &mut impl Storage,
        buffer: &'a mut [u8],
    ) -> Result<Option<(usize, Record<'a>)>, ServiceError> {
        let mut selected = None;
        for (bank, header) in self.valid_banks(flash, buffer.len())?.into_iter().flatten() {
            let length = header.length.get() as usize;
            flash.read(
                bank as u32 * self.bank_size + HEADER_SIZE as u32,
                &mut buffer[..length],
            )?;
            // Check the actual bytes returned to the caller, too.
            if checksum(&header.as_bytes()[..DIGEST_OFFSET], &buffer[..length]) == header.digest {
                selected = Some((bank, header));
                break;
            }
        }
        Ok(selected.map(|(bank, header)| {
            (
                bank,
                Record {
                    key: header.key,
                    sequence: header.sequence.get(),
                    payload: &buffer[..header.length.get() as usize],
                },
            )
        }))
    }

    /// Commit into the inactive bank, preserving the active one on any error.
    /// Returns false when the complete payload/key are already current.
    pub fn save(
        &self,
        flash: &mut impl Storage,
        record: &Record<'_>,
        boot: BootPath,
    ) -> Result<bool, ServiceError> {
        if boot == BootPath::S3Resume
            || record.payload.is_empty()
            || record.payload.len() > self.bank_size as usize - HEADER_SIZE
        {
            return Err(ServiceError::InvalidParam);
        }
        let current = self
            .valid_banks(flash, self.bank_size as usize - HEADER_SIZE)?
            .into_iter()
            .flatten()
            .next();
        // Like coreboot's MRC metadata comparison, compare the verified
        // digest instead of copying another full payload to RAM.
        if current.as_ref().is_some_and(|(_, old)| {
            old.key == record.key
                && old.length.get() as usize == record.payload.len()
                && checksum(&old.as_bytes()[..DIGEST_OFFSET], record.payload) == old.digest
        }) {
            return Ok(false);
        }
        let (bank, sequence) = current.map_or((0, 0), |(bank, old)| {
            (1 - bank, old.sequence.get().wrapping_add(1))
        });
        let mut header = Header {
            magic: MAGIC,
            version: 1.into(),
            sequence: sequence.into(),
            length: (record.payload.len() as u32).into(),
            reserved: 0.into(),
            key: record.key,
            digest: [0; 32],
            commit: u32::MAX.into(),
        };
        header.digest = checksum(&header.as_bytes()[..DIGEST_OFFSET], record.payload());
        let offset = bank as u32 * self.bank_size;
        flash.erase(offset, self.bank_size)?;
        // Bound program requests independently of any controller's transfer size.
        for (chunk, bytes) in header.as_bytes()[..COMMIT_OFFSET].chunks(64).enumerate() {
            flash.program(offset + (chunk * 64) as u32, bytes)?;
        }
        for (chunk, bytes) in record.payload().chunks(64).enumerate() {
            flash.program(offset + HEADER_SIZE as u32 + (chunk * 64) as u32, bytes)?;
        }
        if self.read_bank(flash, bank, false, record.payload.len())? != Some(header) {
            return Err(ServiceError::HardwareError);
        }
        // Commit marker is programmed LAST, only after a full uncached readback.
        flash.program(offset + COMMIT_OFFSET as u32, &[0; 4])?;
        header.commit = 0.into();
        if self.read_bank(flash, bank, true, record.payload.len())? != Some(header) {
            return Err(ServiceError::HardwareError);
        }
        Ok(true)
    }
}
/// Volatile reads from linked flash or reserved RAM. This never provides a
/// programming interface and never trusts a directory as an address authority.
#[cfg(all(feature = "stage", feature = "memory-cache"))]
pub(crate) struct MappedRead {
    base: usize,
    size: usize,
}
#[cfg(all(feature = "stage", feature = "memory-cache"))]
impl MappedRead {
    /// # Safety
    /// The entire window must be mapped/readable and have a trusted extent.
    pub(crate) unsafe fn new(base: usize, size: usize) -> Self {
        Self { base, size }
    }
}
#[cfg(all(feature = "stage", feature = "memory-cache"))]
impl Storage for MappedRead {
    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), ServiceError> {
        let offset = offset as usize;
        if offset
            .checked_add(bytes.len())
            .is_none_or(|end| end > self.size)
        {
            return Err(ServiceError::InvalidParam);
        }
        for (index, byte) in bytes.iter_mut().enumerate() {
            // SAFETY: the constructor and the bounds check cover each byte.
            *byte = unsafe { core::ptr::read_volatile((self.base + offset + index) as *const u8) };
        }
        Ok(())
    }
    fn program(&mut self, _: u32, _: &[u8]) -> Result<(), ServiceError> {
        Err(ServiceError::NotSupported)
    }
    fn erase(&mut self, _: u32, _: u32) -> Result<(), ServiceError> {
        Err(ServiceError::NotSupported)
    }
}

pub const BANK_SIZE: u32 = 0x10000;
pub const EXTENT_SIZE: u32 = 2 * BANK_SIZE;

#[cfg(all(feature = "stage", feature = "memory-cache"))]
pub(crate) fn runtime_key<B: crate::IntelBoard>(
    northbridge: &<B::Platform as crate::IntelEarlyPlatform>::Northbridge,
    southbridge: &<B::Platform as crate::IntelEarlyPlatform>::Southbridge,
    geometry: crate::layout::IntelBootLayout<'static>,
) -> Result<[u8; 32], ServiceError> {
    use fstart_driver_intel::{IntelNorthbridgeDriver, IntelSouthbridgeDriver};
    let southbridge_identity = southbridge
        .training_identity()
        .ok_or(ServiceError::NotSupported)?;
    let identity = northbridge
        .training_identity()
        .ok_or(ServiceError::NotSupported)?;
    let (base, size) = geometry.firmware()?;
    let locator = unsafe {
        fstart_core::ffs::locator::LocatorRef::read_volatile(fstart_stage::fstart_anchor_bytes())
    }
    .ok_or(ServiceError::InvalidParam)?
    .value()
    .media();
    if locator.image_offset != 0 || !locator.validate(size as u64) {
        return Err(ServiceError::InvalidParam);
    }
    let mut root = [0; 512];
    let mut media = unsafe { MappedRead::new(base as usize, size) };
    media.read(
        locator
            .root_offset
            .try_into()
            .map_err(|_| ServiceError::InvalidParam)?,
        &mut root,
    )?;
    let mut hash = Sha256::new();
    hash.update(b"fstart-mrc-policy-v1");
    hash.update(core::any::type_name::<B>().as_bytes());
    hash.update(identity);
    hash.update(southbridge_identity);
    // These bytes invalidate on firmware updates; they are NOT pre-RAM
    // signature authentication or a source of writable flash boundaries.
    hash.update(root);
    Ok(hash.finalize().into())
}

/// Publish into an owned DRAM store entry. The store's final writeback
/// publishes this entry along with the other entries before CAR teardown.
///
/// # Safety
/// `region` must be exclusively owned, mapped RAM, disjoint from `payload`.
/// The caller must write the store back before a transition that could discard
/// dirty cache lines.
#[cfg(any(all(feature = "stage", feature = "memory-cache"), test))]
pub(crate) unsafe fn publish_pending(
    region: fstart_core::layout::Region,
    key: [u8; 32],
    payload: &[u8],
) -> Result<(), ServiceError> {
    if payload.is_empty()
        || u32::try_from(payload.len()).is_err()
        || HEADER_SIZE
            .checked_add(payload.len())
            .is_none_or(|size| region.size < size as u64)
        || region.base % 4 != 0
    {
        return Err(ServiceError::InvalidParam);
    }
    let mut header = Header {
        magic: MAGIC,
        version: 1.into(),
        sequence: 0.into(),
        length: (payload.len() as u32).into(),
        reserved: 0.into(),
        key,
        digest: [0; 32],
        commit: u32::MAX.into(),
    };
    header.digest = checksum(&header.as_bytes()[..DIGEST_OFFSET], payload);
    let base = region.base as *mut u8;
    // The linked OS-reserved handoff is written only after successful cold
    // memory testing; the owner flushes the whole store before teardown.
    unsafe {
        core::ptr::write_volatile(base.add(COMMIT_OFFSET).cast::<u32>(), u32::MAX);
        for (offset, byte) in header.as_bytes()[..COMMIT_OFFSET]
            .iter()
            .chain(payload)
            .enumerate()
        {
            // Payload follows the whole header, not its omitted commit word.
            let offset = if offset >= COMMIT_OFFSET {
                offset + 4
            } else {
                offset
            };
            core::ptr::write_volatile(base.add(offset), *byte);
        }
        core::ptr::write_volatile(base.add(COMMIT_OFFSET).cast::<u32>(), 0);
    }
    Ok(())
}

#[cfg(all(feature = "stage", feature = "memory-cache"))]
pub(crate) fn commit_pending<B: crate::IntelBoard>(
    northbridge: &<B::Platform as crate::IntelEarlyPlatform>::Northbridge,
    southbridge: &<B::Platform as crate::IntelEarlyPlatform>::Southbridge,
    geometry: crate::layout::IntelBootLayout<'static>,
    store: &fstart_store::Store,
) -> Result<(), ServiceError> {
    use fstart_driver_intel::southbridge::spi::{CacheSpi, FlashExtent};
    if !fstart_stage::directory::matches_raw_reservation("mrc-cache", 0, EXTENT_SIZE, 0xff) {
        return Err(ServiceError::InvalidParam);
    }
    // The bootblock publishes a record only after training; a replayed boot
    // has nothing to commit.
    let pending = store
        .find(fstart_store::tag::TRAINING)
        .ok_or(ServiceError::NotInitialized)?;
    // SAFETY: this entry belongs to the store and only the boot CPU accesses
    // its data here. Borrow the DRAM payload; do not allocate another copy.
    let bytes = unsafe { store.bytes_mut(&pending) };
    let record = Record::parse(bytes)?;
    if record.key != runtime_key::<B>(northbridge, southbridge, geometry)? {
        return Err(ServiceError::HardwareError);
    }
    let bios_offset = match B::FACTS.flash {
        fstart_core::FlashLayout::IntelIfd(layout) => {
            layout
                .bios_region()
                .ok_or(ServiceError::InvalidParam)?
                .offset
        }
        fstart_core::FlashLayout::X86Legacy(_) => 0,
    };
    // Constructing the controller is deliberately confined to ramstage; its
    // dependency's code and constants reside in the RAM-linked image.
    let mut flash = unsafe {
        CacheSpi::new(FlashExtent {
            offset: bios_offset,
            size: EXTENT_SIZE,
            chip_size: B::FACTS.flash_size,
        })?
    };
    let journal = Journal::new(BANK_SIZE, EXTENT_SIZE)?;
    let changed = journal.save(&mut flash, &record, BootPath::Normal)?;
    fstart_log::info!(
        "memory cache: {}",
        if changed {
            "committed and verified"
        } else {
            "already current"
        }
    );
    Ok(())
}

fn checksum(header_prefix: &[u8], payload: &[u8]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(header_prefix);
    hash.update(payload);
    hash.finalize().into()
}
fn newer(candidate: u32, reference: u32) -> bool {
    (candidate.wrapping_sub(reference) as i32) > 0
}

#[cfg(all(
    feature = "memory-cache",
    any(target_arch = "x86", target_arch = "x86_64")
))]
impl Storage for fstart_driver_intel::southbridge::spi::CacheSpi {
    fn read(&mut self, o: u32, b: &mut [u8]) -> Result<(), ServiceError> {
        self.read(o, b)
    }
    fn program(&mut self, o: u32, b: &[u8]) -> Result<(), ServiceError> {
        self.program(o, b)
    }
    fn erase(&mut self, o: u32, n: u32) -> Result<(), ServiceError> {
        self.erase(o, n)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec;
    use std::vec::Vec;

    #[derive(Clone)]
    struct Flash {
        bytes: Vec<u8>,
        cut: Option<usize>,
        mutations: usize,
        read_failure: Option<u32>,
    }
    impl Flash {
        fn new() -> Self {
            Self {
                bytes: vec![0xff; 8192],
                cut: None,
                mutations: 0,
                read_failure: None,
            }
        }
        fn fail(&mut self) -> bool {
            let fail = self.cut == Some(self.mutations);
            self.mutations += 1;
            fail
        }
    }
    impl Storage for Flash {
        fn read(&mut self, o: u32, b: &mut [u8]) -> Result<(), ServiceError> {
            if self
                .read_failure
                .is_some_and(|at| o <= at && at < o + b.len() as u32)
            {
                return Err(ServiceError::HardwareError);
            }
            b.copy_from_slice(&self.bytes[o as usize..o as usize + b.len()]);
            Ok(())
        }
        fn program(&mut self, o: u32, b: &[u8]) -> Result<(), ServiceError> {
            let cut = self.fail();
            let length = if cut { b.len() / 2 } else { b.len() };
            for (destination, value) in self.bytes[o as usize..].iter_mut().zip(&b[..length]) {
                assert_eq!(*destination & *value, *value);
                *destination &= value;
            }
            if cut {
                Err(ServiceError::HardwareError)
            } else {
                Ok(())
            }
        }
        fn erase(&mut self, o: u32, n: u32) -> Result<(), ServiceError> {
            let cut = self.fail();
            let length = if cut { n / 2 } else { n };
            self.bytes[o as usize..(o + length) as usize].fill(0xff);
            if cut {
                Err(ServiceError::HardwareError)
            } else {
                Ok(())
            }
        }
    }
    #[test]
    fn journal_wire_layout_is_unchanged() {
        assert_eq!(HEADER_SIZE, 92);
        assert_eq!(DIGEST_OFFSET, 56);
        assert_eq!(COMMIT_OFFSET, 88);
    }

    #[test]
    fn workspace_is_one_shot_and_input_does_not_alias_capture() {
        static WORKSPACE: TrainingWorkspace<37> = TrainingWorkspace::new();
        let TrainingBuffers { cached, capture } = WORKSPACE.take().unwrap();
        assert_eq!(cached.len(), 37);
        assert_eq!(capture.len(), 37);
        cached.fill(5);
        assert!(capture.iter().all(|byte| *byte == 0));
        capture.fill(9);
        assert!(cached.iter().all(|byte| *byte == 5));
        assert!(WORKSPACE.take().is_none());
    }

    #[test]
    fn records_borrow_payloads_and_the_caller_bounds_loading() {
        assert!(core::mem::size_of::<Record<'_>>() <= 64);
        let mut flash = Flash::new();
        let payload = vec![9; 1537];
        let large = Record::new([5; 32], &payload).unwrap();
        assert_eq!(large.payload().as_ptr(), payload.as_ptr());
        assert_eq!(large.encoded_size(), HEADER_SIZE + payload.len());
        let journal = Journal::new(4096, 8192).unwrap();
        journal.save(&mut flash, &large, BootPath::Normal).unwrap();
        let mut small = [0; 37];
        assert!(journal.load(&mut flash, &mut small).unwrap().is_none());
        let mut buffer = vec![0; payload.len()];
        let address = buffer.as_ptr();
        let loaded = journal.load(&mut flash, &mut buffer).unwrap().unwrap().1;
        assert_eq!(loaded.payload(), payload);
        assert_eq!(loaded.payload().as_ptr(), address);
        let parsed = Record::parse(&flash.bytes[..large.encoded_size()]).unwrap();
        assert_eq!(parsed.payload(), payload);
        assert_eq!(
            parsed.payload().as_ptr(),
            flash.bytes[HEADER_SIZE..].as_ptr()
        );
    }

    #[test]
    fn same_key_payload_changes_are_saved_and_digest_comparison_skips_duplicates() {
        let journal = Journal::new(4096, 8192).unwrap();
        let mut flash = Flash::new();
        let first = Record::new([3; 32], b"first").unwrap();
        let second = Record::new([3; 32], b"other").unwrap();
        assert!(journal.save(&mut flash, &first, BootPath::Normal).unwrap());
        assert!(journal.save(&mut flash, &second, BootPath::Normal).unwrap());
        let before = flash.mutations;
        assert!(!journal.save(&mut flash, &second, BootPath::Normal).unwrap());
        assert_eq!(flash.mutations, before);
        let mut buffer = [0; 31];
        let loaded = journal.load(&mut flash, &mut buffer).unwrap().unwrap().1;
        assert_eq!(loaded.sequence, 1);
        assert_eq!(loaded.payload(), second.payload());
    }

    #[test]
    fn bounded_loading_and_corruption_fall_back_but_io_errors_propagate() {
        let journal = Journal::new(4096, 8192).unwrap();
        let mut flash = Flash::new();
        let small = Record::new([1; 32], b"small").unwrap();
        let bytes = vec![7; 1537];
        let large = Record::new([2; 32], &bytes).unwrap();
        journal.save(&mut flash, &small, BootPath::Normal).unwrap();
        journal.save(&mut flash, &large, BootPath::Normal).unwrap();
        let mut buffer = vec![0; bytes.len()];
        assert_eq!(
            journal
                .load(&mut flash, &mut buffer[..37])
                .unwrap()
                .unwrap()
                .1
                .key,
            small.key
        );
        assert_eq!(
            journal
                .load(&mut flash, &mut buffer)
                .unwrap()
                .unwrap()
                .1
                .key,
            large.key
        );
        for at in [0, HEADER_SIZE as u32, 4096, 4096 + HEADER_SIZE as u32] {
            flash.read_failure = Some(at);
            assert!(matches!(
                journal.load(&mut flash, &mut buffer),
                Err(ServiceError::HardwareError)
            ));
            let before = flash.mutations;
            assert!(journal.save(&mut flash, &large, BootPath::Normal).is_err());
            assert_eq!(flash.mutations, before);
        }
        flash.read_failure = None;
        flash.bytes[4096 + HEADER_SIZE] ^= 1;
        assert_eq!(
            journal
                .load(&mut flash, &mut buffer)
                .unwrap()
                .unwrap()
                .1
                .key,
            small.key
        );
    }

    #[test]
    fn pending_handoff_uses_the_encoded_length_without_a_fixed_slot_size() {
        use fstart_core::layout::{Region, RegionKind};
        #[repr(align(8))]
        struct Pending([u8; 512]);
        let mut pending = Pending([0xa5; 512]);
        let payload = [7; 317];
        let record = Record::new([3; 32], &payload).unwrap();
        let region = Region {
            kind: RegionKind::FirmwareStore,
            base: pending.0.as_mut_ptr() as u64,
            size: record.encoded_size() as u64,
        };
        // SAFETY: the aligned buffer is exclusively owned and disjoint from payload.
        unsafe { publish_pending(region, record.key, record.payload()).unwrap() };
        let parsed = Record::parse(&pending.0[..record.encoded_size()]).unwrap();
        assert_eq!(parsed.key, record.key);
        assert_eq!(parsed.payload(), record.payload());
        assert!(
            pending.0[record.encoded_size()..]
                .iter()
                .all(|byte| *byte == 0xa5)
        );
        let before = pending.0;
        let short = Region {
            size: region.size - 1,
            ..region
        };
        // SAFETY: same owned buffer, with a deliberately too-short bound.
        assert!(unsafe { publish_pending(short, record.key, record.payload()) }.is_err());
        assert_eq!(pending.0, before);
    }

    #[test]
    fn geometry_records_and_resume_policy() {
        assert!(Journal::new(4096, 4096).is_err());
        assert!(Journal::new(2048, 4096).is_err());
        assert!(Journal::new(1 << 31, 0).is_err());
        assert!(Record::new([0; 32], &[]).is_err());
        let journal = Journal::new(4096, 8192).unwrap();
        let mut flash = Flash::new();
        let record = Record::new([3; 32], b"SPD and training").unwrap();
        let mut buffer = [0; 53];
        assert!(journal.load(&mut flash, &mut buffer).unwrap().is_none());
        assert!(
            journal
                .save(&mut flash, &record, BootPath::S3Resume)
                .is_err()
        );
        assert!(journal.save(&mut flash, &record, BootPath::Normal).unwrap());
        assert!(!journal.save(&mut flash, &record, BootPath::Normal).unwrap());
        let loaded = journal.load(&mut flash, &mut buffer).unwrap().unwrap().1;
        assert_eq!(loaded.payload(), record.payload());
        assert_eq!(loaded.key, record.key);
        flash.bytes[HEADER_SIZE] ^= 1;
        assert!(journal.load(&mut flash, &mut buffer).unwrap().is_none());
        let oversized = [0; 4096];
        let before = flash.mutations;
        assert!(
            journal
                .save(
                    &mut flash,
                    &Record::new([0; 32], &oversized).unwrap(),
                    BootPath::Normal
                )
                .is_err()
        );
        assert_eq!(flash.mutations, before);
    }
    #[test]
    fn interrupted_inactive_bank_never_destroys_active_record() {
        for length in [1, 37, 913, 1500, 4096 - HEADER_SIZE] {
            check_interrupted_bank(length);
        }
    }

    fn check_interrupted_bank(length: usize) {
        let journal = Journal::new(4096, 8192).unwrap();
        let old_bytes = vec![1; length];
        let new_bytes = vec![2; length];
        let old = Record::new([1; 32], &old_bytes).unwrap();
        let new = Record::new([2; 32], &new_bytes).unwrap();
        let mut buffer = vec![0; length];
        let mut baseline = Flash::new();
        journal.save(&mut baseline, &old, BootPath::Normal).unwrap();
        baseline.mutations = 0;
        let mut successful = baseline.clone();
        journal
            .save(&mut successful, &new, BootPath::Normal)
            .unwrap();
        for boundary in 0..successful.mutations {
            let mut cut = baseline.clone();
            cut.cut = Some(boundary);
            assert!(journal.save(&mut cut, &new, BootPath::Normal).is_err());
            let loaded = journal.load(&mut cut, &mut buffer).unwrap().unwrap().1;
            assert_eq!(loaded.key, old.key);
            assert_eq!(loaded.payload(), old.payload());
        }
        assert_eq!(
            journal
                .load(&mut successful, &mut buffer)
                .unwrap()
                .unwrap()
                .1
                .payload(),
            new.payload()
        );
    }
    #[test]
    fn rejects_uncommitted_malformed_and_truncated_record_headers() {
        let journal = Journal::new(4096, 8192).unwrap();
        let record = Record::new([7; 32], b"training").unwrap();
        let mut buffer = [0; 47];
        let mut baseline = Flash::new();
        journal
            .save(&mut baseline, &record, BootPath::Normal)
            .unwrap();
        for (offset, bytes) in [
            (0, &[0; 8][..]),
            (8, &[2, 0, 0, 0][..]),
            (16, &u32::MAX.to_le_bytes()[..]),
            (20, &[1, 0, 0, 0][..]),
            (COMMIT_OFFSET, &[0xff; 4][..]),
            (DIGEST_OFFSET, &[0; 32][..]),
        ] {
            let mut bad = baseline.clone();
            bad.bytes[offset..offset + bytes.len()].copy_from_slice(bytes);
            assert!(journal.load(&mut bad, &mut buffer).unwrap().is_none());
            assert!(Record::parse(&bad.bytes[..4096]).is_err());
        }
        for length in 0..HEADER_SIZE + record.payload().len() {
            let mut truncated = baseline.clone();
            truncated.bytes[length..4096].fill(0xff);
            assert!(journal.load(&mut truncated, &mut buffer).unwrap().is_none());
            assert!(Record::parse(&baseline.bytes[..length]).is_err());
        }
        assert!(newer(0, u32::MAX));
        assert!(!newer(u32::MAX, 0));
    }
}
