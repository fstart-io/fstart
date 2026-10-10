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

pub const MAX_PAYLOAD: usize = 768;
const MAGIC: [u8; 8] = *b"FSTMRC01";

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout)]
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

/// Narrow storage contract; every offset is relative to the trusted cache
/// extent. The backend enforces physical bounds, chip geometry and protection.
pub trait Storage {
    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), ServiceError>;
    fn program(&mut self, offset: u32, bytes: &[u8]) -> Result<(), ServiceError>;
    fn erase(&mut self, offset: u32, length: u32) -> Result<(), ServiceError>;
}

#[derive(Clone, PartialEq, Eq)]
pub struct Record {
    pub key: [u8; 32],
    pub sequence: u32,
    payload: [u8; MAX_PAYLOAD],
    length: usize,
}
impl Record {
    pub const fn empty() -> Self {
        Self {
            key: [0; 32],
            sequence: 0,
            payload: [0; MAX_PAYLOAD],
            length: 0,
        }
    }
    pub fn new(key: [u8; 32], payload: &[u8]) -> Result<Self, ServiceError> {
        if payload.is_empty() || payload.len() > MAX_PAYLOAD {
            return Err(ServiceError::InvalidParam);
        }
        let mut record = Self::empty();
        record.key = key;
        record.length = payload.len();
        record.payload[..payload.len()].copy_from_slice(payload);
        Ok(record)
    }
    pub fn payload(&self) -> &[u8] {
        &self.payload[..self.length]
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

    fn read_bank(
        &self,
        flash: &mut impl Storage,
        bank: usize,
        committed: bool,
    ) -> Result<Option<Record>, ServiceError> {
        let offset = bank as u32 * self.bank_size;
        let mut bytes = [0u8; HEADER_SIZE];
        flash.read(offset, &mut bytes)?;
        let header = Header::ref_from_bytes(&bytes).map_err(|_| ServiceError::InvalidParam)?;
        let length = header.length.get() as usize;
        if header.magic != MAGIC
            || header.version.get() != 1
            || header.reserved.get() != 0
            || length == 0
            || length > MAX_PAYLOAD
            || HEADER_SIZE + length > self.bank_size as usize
            || header.commit.get() != if committed { 0 } else { u32::MAX }
        {
            return Ok(None);
        }
        let mut record = Record::empty();
        record.key = header.key;
        record.sequence = header.sequence.get();
        record.length = length;
        flash.read(offset + HEADER_SIZE as u32, &mut record.payload[..length])?;
        let digest = checksum(&bytes[..DIGEST_OFFSET], record.payload());
        if digest != header.digest {
            return Ok(None);
        }
        Ok(Some(record))
    }

    /// Load the newest complete record. Storage failures are not cache misses.
    /// Callers must independently match `key` and decode/validate the payload;
    /// on S3, any mismatch or miss must reset rather than cold-train retained RAM.
    pub fn load(&self, flash: &mut impl Storage) -> Result<Option<(usize, Record)>, ServiceError> {
        let first = self.read_bank(flash, 0, true)?;
        let second = self.read_bank(flash, 1, true)?;
        Ok(match (first, second) {
            (None, None) => None,
            (Some(a), None) => Some((0, a)),
            (None, Some(b)) => Some((1, b)),
            (Some(a), Some(b)) => {
                if newer(b.sequence, a.sequence) {
                    Some((1, b))
                } else {
                    Some((0, a))
                }
            }
        })
    }

    /// Commit into the inactive bank, preserving the active one on any error.
    /// Returns false when the complete payload/key are already current.
    pub fn save(
        &self,
        flash: &mut impl Storage,
        record: &Record,
        boot: BootPath,
    ) -> Result<bool, ServiceError> {
        if boot == BootPath::S3Resume || record.length == 0 || record.length > MAX_PAYLOAD {
            return Err(ServiceError::InvalidParam);
        }
        let current = self.load(flash)?;
        if current
            .as_ref()
            .is_some_and(|(_, old)| old.key == record.key && old.payload() == record.payload())
        {
            return Ok(false);
        }
        let (bank, sequence) = current.map_or((0, 0), |(bank, old)| {
            (1 - bank, old.sequence.wrapping_add(1))
        });
        let mut header = Header {
            magic: MAGIC,
            version: 1.into(),
            sequence: sequence.into(),
            length: (record.length as u32).into(),
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
        let verified = self
            .read_bank(flash, bank, false)?
            .ok_or(ServiceError::HardwareError)?;
        if verified.key != record.key
            || verified.sequence != sequence
            || verified.payload() != record.payload()
        {
            return Err(ServiceError::HardwareError);
        }
        // Commit marker is programmed LAST, only after a full uncached readback.
        flash.program(offset + COMMIT_OFFSET as u32, &[0; 4])?;
        self.read_bank(flash, bank, true)?
            .ok_or(ServiceError::HardwareError)?;
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

#[cfg(all(feature = "stage", feature = "memory-cache", fstart_stage_env = "car"))]
pub(crate) unsafe fn publish_pending(
    region: fstart_core::layout::Region,
    key: [u8; 32],
    payload: &[u8],
) -> Result<(), ServiceError> {
    if payload.is_empty()
        || payload.len() > MAX_PAYLOAD
        || region.size < (HEADER_SIZE + payload.len()) as u64
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
    // memory testing. CAR teardown must not discard dirty cached copies.
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
        // Some chipset memory tests already enable WB MTRRs. INVD must not
        // discard this record with CAR, regardless of the current RAM type.
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            core::arch::asm!("mfence", options(nostack, preserves_flags));
            for offset in (0..HEADER_SIZE + payload.len()).step_by(64) {
                core::arch::asm!("clflush [{}]", in(reg) base.add(offset), options(nostack, preserves_flags));
            }
            core::arch::asm!("mfence", options(nostack, preserves_flags));
        }
    }
    Ok(())
}

#[cfg(all(feature = "stage", feature = "memory-cache", fstart_stage_env = "ram"))]
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
    let mut ram = unsafe { MappedRead::new(store.address(&pending), pending.len()) };
    let record = Journal {
        bank_size: pending.len() as u32,
    }
    .read_bank(&mut ram, 0, true)?
    .ok_or(ServiceError::HardwareError)?;
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
    }
    impl Flash {
        fn new() -> Self {
            Self {
                bytes: vec![0xff; 8192],
                cut: None,
                mutations: 0,
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
    fn geometry_records_and_resume_policy() {
        assert!(Journal::new(4096, 4096).is_err());
        assert!(Journal::new(2048, 4096).is_err());
        assert!(Journal::new(1 << 31, 0).is_err());
        assert!(Record::new([0; 32], &[]).is_err());
        assert!(Record::new([0; 32], &[0; MAX_PAYLOAD + 1]).is_err());
        let journal = Journal::new(4096, 8192).unwrap();
        let mut flash = Flash::new();
        let record = Record::new([3; 32], b"SPD and training").unwrap();
        assert!(journal.load(&mut flash).unwrap().is_none());
        assert!(
            journal
                .save(&mut flash, &record, BootPath::S3Resume)
                .is_err()
        );
        assert!(journal.save(&mut flash, &record, BootPath::Normal).unwrap());
        assert!(!journal.save(&mut flash, &record, BootPath::Normal).unwrap());
        let loaded = journal.load(&mut flash).unwrap().unwrap().1;
        assert_eq!(loaded.payload(), record.payload());
        assert_eq!(loaded.key, record.key);
        flash.bytes[HEADER_SIZE] ^= 1;
        assert!(journal.load(&mut flash).unwrap().is_none());
    }
    #[test]
    fn interrupted_inactive_bank_never_destroys_active_record() {
        let journal = Journal::new(4096, 8192).unwrap();
        let old = Record::new([1; 32], &[1; MAX_PAYLOAD]).unwrap();
        let new = Record::new([2; 32], &[2; MAX_PAYLOAD]).unwrap();
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
            let loaded = journal.load(&mut cut).unwrap().unwrap().1;
            assert_eq!(loaded.key, old.key);
            assert_eq!(loaded.payload(), old.payload());
        }
        assert_eq!(
            journal.load(&mut successful).unwrap().unwrap().1.payload(),
            new.payload()
        );
    }
    #[test]
    fn rejects_uncommitted_malformed_and_truncated_record_headers() {
        let journal = Journal::new(4096, 8192).unwrap();
        let record = Record::new([7; 32], b"training").unwrap();
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
            assert!(journal.load(&mut bad).unwrap().is_none());
        }
        for length in 0..HEADER_SIZE + record.payload().len() {
            let mut truncated = baseline.clone();
            truncated.bytes[length..4096].fill(0xff);
            assert!(journal.load(&mut truncated).unwrap().is_none());
        }
        assert!(newer(0, u32::MAX));
        assert!(!newer(u32::MAX, 0));
    }
}
