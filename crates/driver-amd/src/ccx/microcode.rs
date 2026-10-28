// SPDX-License-Identifier: MIT OR Apache-2.0
//! Flat microcode patches, as the BIOS directory (type 0x66 entries) and the
//! FFS carry them, not AMD container or signed-wrapper parsing. A patch is
//! for the CPUs whose equivalence ID its header names (+0x18), and that ID
//! sizes it ([`fstart_amdfw_format::microcode::patch_size`]): 0xc80 bytes on
//! Family 17h, 0x15c0 on Milan.
//!
//! A patch that names a northbridge or southbridge is rejected (fail closed)
//! rather than matched against bus 0: no vendor patch for these SoCs does.
//! The SMT siblings of a core share its patch: load on one thread per core.
use core::marker::PhantomData;
use fstart_amdfw_format::microcode as format;
use fstart_arch::x86::cpu::msr::{Msr, MsrAccess, PATCH_LEVEL};
use tock_registers::interfaces::{Readable, Writeable};
use zerocopy::{
    FromBytes, Immutable, KnownLayout,
    byteorder::{LittleEndian, U16, U32},
};

/// Writing a patch's physical address loads it.
pub const PATCH_LOADER: Msr = Msr::new(0xc001_0020);
/// The loaded patch's address, for the PSP's reload on S3 resume (SP3).
pub const PATCH_ADDR_S3: Msr = Msr::new(0xc001_100c);
/// Bit 7: the patch load of ZP A0-B1 needs it set around the load.
const LS_CFG2: Msr = Msr::new(0xc001_102d);
/// Bit 0: set on ZP B1 with a patch newer than 0x08001117.
const C001_100B: Msr = Msr::new(0xc001_100b);

/// CPUID(1).EAX of the Zeppelin steppings with load quirks.
const ZP_A0: u32 = 0x0080_0f00;
const ZP_B0: u32 = 0x0080_0f10;
const ZP_B1: u32 = 0x0080_0f11;

/// The CPUs a SoC's patch bundle is for.
pub trait Family {
    /// CPUID(1).EAX of the supported models, stepping masked off.
    const SIGNATURES: &'static [u32];
    /// Firmware input budget, not silicon discovery.
    const MAX_PATCHES: usize;
}

#[repr(C)]
#[derive(FromBytes, Immutable, KnownLayout)]
struct Header {
    date_code: U32<LittleEndian>,
    patch_id: U32<LittleEndian>,
    patch_data_id: U16<LittleEndian>,
    reserved1: [u8; 6],
    chipset1: U32<LittleEndian>,
    chipset2: U32<LittleEndian>,
    processor_rev: U16<LittleEndian>,
    chipset1_rev: u8,
    chipset2_rev: u8,
    reserved2: [u8; 4],
}
const _: () = assert!(core::mem::size_of::<Header>() == 32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Format,
    CpuIdentity,
    ProcessorRevision { expected: u16, actual: u16 },
    Address,
    Rejected { expected: u32, actual: u32 },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    AlreadyCurrent(u32),
    Installed(u32),
}

/// The equivalence ID of a CPUID(1).EAX signature (e.g. 0x8012 for ZP-B2).
pub const fn equivalence_id(signature: u32) -> u16 {
    ((signature & 0xff0000) >> 8 | (signature & 0xff)) as u16
}

/// A patch's header, if `bytes` are exactly one patch for no particular
/// chipset.
fn header(bytes: &[u8]) -> Result<Header, Error> {
    let size = format::equivalence_id(bytes).and_then(format::patch_size);
    if size != Some(bytes.len()) {
        return Err(Error::Format);
    }
    let (header, _) = Header::read_from_prefix(bytes).map_err(|_| Error::Format)?;
    if header.patch_id.get() == 0
        || header.chipset1.get() != 0
        || header.chipset2.get() != 0
        || header.chipset1_rev != 0
        || header.chipset2_rev != 0
    {
        return Err(Error::Format);
    }
    Ok(header)
}

/// Concatenated unchanged flat patches. No wrapper, implicit best-version
/// choice, duplicate equivalence IDs or ignored malformed non-selected
/// entries. Format validation and exact selection are NOT
/// authentication/freshness proof.
pub struct Bundle<'a, F> {
    bytes: &'a [u8],
    family: PhantomData<F>,
}
impl<'a, F: Family> Bundle<'a, F> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        let mut count = 0;
        for (index, patch) in format::patches(bytes).enumerate() {
            let revision = header(patch.map_err(|_| Error::Format)?)?
                .processor_rev
                .get();
            let duplicate = format::patches(bytes)
                .take(index)
                .flatten()
                .any(|other| header(other).is_ok_and(|h| h.processor_rev.get() == revision));
            if revision == 0 || duplicate {
                return Err(Error::Format);
            }
            count += 1;
        }
        if count == 0 || count > F::MAX_PATCHES {
            return Err(Error::Format);
        }
        Ok(Self {
            bytes,
            family: PhantomData,
        })
    }
    /// The patch for the CPU with `signature`.
    pub fn select(&self, signature: u32) -> Result<Patch<'a>, Error> {
        if !F::SIGNATURES.contains(&(signature & !0xf)) {
            return Err(Error::CpuIdentity);
        }
        for bytes in format::patches(self.bytes).flatten() {
            match Patch::parse(bytes, signature) {
                Ok(patch) => return Ok(patch),
                Err(Error::ProcessorRevision { .. }) => {}
                Err(error) => return Err(error),
            }
        }
        Err(Error::CpuIdentity)
    }
}

/// The first of `candidates` (e.g. BIOS directory 0x66 entries) that is a
/// valid patch for the CPU with `signature`.
pub fn select<'a>(
    signature: u32,
    candidates: impl IntoIterator<Item = &'a [u8]>,
) -> Option<Patch<'a>> {
    candidates
        .into_iter()
        .find_map(|bytes| Patch::parse(bytes, signature).ok())
}

/// How a load is done.
#[derive(Debug, Clone, Copy)]
pub struct Load {
    /// Leave the patch address in [`PATCH_ADDR_S3`] for the PSP to reload
    /// on S3 resume (SP3).
    pub record_for_s3: bool,
    /// Write back and invalidate the caches (ZP A0-B1).
    pub writeback: unsafe fn(),
}
impl Default for Load {
    fn default() -> Self {
        Self {
            record_for_s3: false,
            writeback: fstart_arch::x86::writeback_invalidate_caches,
        }
    }
}

/// An immutable vendor patch view. Parsing is NOT signature authentication.
#[derive(Clone, Copy)]
pub struct Patch<'a> {
    bytes: &'a [u8],
    signature: u32,
    revision: u32,
}
impl<'a> Patch<'a> {
    pub fn parse(bytes: &'a [u8], signature: u32) -> Result<Self, Error> {
        let header = header(bytes)?;
        let expected = equivalence_id(signature);
        let actual = header.processor_rev.get();
        if actual != expected {
            return Err(Error::ProcessorRevision { expected, actual });
        }
        Ok(Self {
            bytes,
            signature,
            revision: header.patch_id.get(),
        })
    }
    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }
    pub fn revision(&self) -> u32 {
        self.revision
    }
    /// The bytes' address, where they are identity mapped.
    pub fn address(&self) -> u64 {
        self.bytes.as_ptr() as u64
    }

    /// Validate the whole below-4GiB, 16-byte-aligned physical read extent.
    /// Numeric geometry alone does not prove it addresses the parsed bytes.
    pub fn validate_address(&self, address: u64) -> Result<(), Error> {
        if address == 0
            || address & 15 != 0
            || address
                .checked_add(self.bytes.len() as u64)
                .is_none_or(|end| end > 1 << 32)
        {
            return Err(Error::Address);
        }
        Ok(())
    }

    /// Load once and verify the installed patch level. A newer installed
    /// revision is retained, never downgraded. No rejected-load retry. The
    /// ZP A0-B1 load quirks follow from the signature.
    ///
    /// # Safety
    /// Supported CPU at ring 0 with caching on; serialize all CPU/sibling
    /// patch operations and exclude asynchronous firmware work. `address`
    /// must address these exact trusted vendor bytes in coherent,
    /// CPU-readable physical memory. Keep the complete allocation immutable,
    /// mapped and reserved through reset. Do not pass a flash-XIP/virtual-only
    /// address or assume parsing authenticates it. A fault or rejected
    /// submitted load requires platform fail-stop/reset.
    pub unsafe fn apply<M: MsrAccess + ?Sized>(
        &self,
        cpu: &M,
        address: u64,
        signature: u32,
        load: Load,
    ) -> Result<Outcome, Error> {
        if signature != self.signature {
            return Err(Error::CpuIdentity);
        }
        self.validate_address(address)?;
        let current = cpu.reg(PATCH_LEVEL).get() as u32;
        if current >= self.revision {
            // The installed patch is this one or newer: register this image
            // for the S3 reload only if it is the installed patch.
            if current == self.revision && load.record_for_s3 {
                cpu.reg(PATCH_ADDR_S3).set(0);
                cpu.reg(PATCH_ADDR_S3).set(address);
            }
            after_load(cpu, signature, current);
            return Ok(Outcome::AlreadyCurrent(current));
        }
        let early_zp = matches!(signature, ZP_A0 | ZP_B0 | ZP_B1);
        if early_zp {
            cpu.reg(LS_CFG2).set(cpu.reg(LS_CFG2).get() | 1 << 7);
        }
        if load.record_for_s3 {
            cpu.reg(PATCH_ADDR_S3).set(0);
            cpu.reg(PATCH_ADDR_S3).set(address);
        }
        cpu.reg(PATCH_LOADER).set(address);
        if early_zp {
            // SAFETY: the caller's contract: ring 0, caching on.
            unsafe { (load.writeback)() };
            cpu.reg(LS_CFG2).set(cpu.reg(LS_CFG2).get() & !(1 << 7));
        }
        let actual = cpu.reg(PATCH_LEVEL).get() as u32;
        if actual != self.revision {
            return Err(Error::Rejected {
                expected: self.revision,
                actual,
            });
        }
        after_load(cpu, signature, actual);
        Ok(Outcome::Installed(actual))
    }
}

/// What a core needs once it runs patch `level`, whoever loaded it.
fn after_load<M: MsrAccess + ?Sized>(cpu: &M, signature: u32, level: u32) {
    if signature == ZP_B1 && level > 0x0800_1117 {
        cpu.reg(C001_100B).set(cpu.reg(C001_100B).get() | 1);
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use core::cell::RefCell;
    use fstart_arch::x86::cpu::msr::testing::{MsrModel, MsrSpace};
    use std::vec::Vec;
    // The Picasso PCO B0/B1 and RV2 A0 inputs.
    enum Test {}
    impl Family for Test {
        const SIGNATURES: &'static [u32] = &[0x00810f10, 0x00810f80, 0x00820f00];
        const MAX_PATCHES: usize = 3;
    }
    const MAX_PATCHES: usize = Test::MAX_PATCHES;
    type Bundle<'a> = super::Bundle<'a, Test>;
    const FAMILY_17H: usize = 0xc80;
    const MILAN: usize = 0x15c0;
    /// Loads take effect: PATCH_LOADER sets the patch level to `loaded`.
    struct Cpu {
        msrs: MsrSpace,
        loaded: u32,
    }
    impl MsrModel for Cpu {
        fn read(&mut self, index: u32) -> u64 {
            self.msrs.read(index)
        }
        fn write(&mut self, index: u32, value: u64) {
            if index == PATCH_LOADER.index() {
                self.msrs
                    .values
                    .insert(PATCH_LEVEL.index(), u64::from(self.loaded));
            }
            self.msrs.write(index, value)
        }
    }
    fn cpu(level: u32, loaded: u32) -> RefCell<Cpu> {
        let mut msrs = MsrSpace::default();
        msrs.values.insert(PATCH_LEVEL.index(), u64::from(level));
        RefCell::new(Cpu { msrs, loaded })
    }
    fn writes(cpu: &RefCell<Cpu>) -> Vec<u32> {
        let writes = core::mem::take(&mut cpu.borrow_mut().msrs.writes);
        writes.iter().map(|w| w.0).collect()
    }
    fn bytes(signature: u32, revision: u32, size: usize) -> Vec<u8> {
        let mut bytes = std::vec![0; size];
        bytes[4..8].copy_from_slice(&revision.to_le_bytes());
        bytes[24..26].copy_from_slice(&equivalence_id(signature).to_le_bytes());
        bytes
    }
    #[test]
    fn bundle_preflights_every_entry_and_selects_without_fallback() {
        let a = bytes(0x00810f80, 0x08108002, FAMILY_17H);
        let b = bytes(0x00820f01, 0x08200105, FAMILY_17H);
        let mut data = a.clone();
        data.extend_from_slice(&b);
        let bundle = Bundle::parse(&data).unwrap();
        assert_eq!(bundle.select(0x00820f01).unwrap().bytes(), b);
        assert!(bundle.select(0x00810f81).is_err());
        assert!(bundle.select(0x000106c2).is_err());
        data[FAMILY_17H + 16] = 1;
        assert!(Bundle::parse(&data).is_err()); // even when selecting valid entry0
        data[FAMILY_17H..].copy_from_slice(&a);
        assert!(Bundle::parse(&data).is_err()); // no ambiguous version choice
        assert!(Bundle::parse(&[]).is_err());
        assert!(Bundle::parse(&data[..data.len() - 1]).is_err());
        let many: Vec<u8> = (0..=MAX_PATCHES as u32)
            .flat_map(|n| bytes(0x00810f80 + n, 0x08108002, FAMILY_17H))
            .collect();
        assert!(Bundle::parse(&many).is_err());
    }
    #[test]
    fn selection_rejects_wrong_cpu_length_and_chipset() {
        assert_eq!(equivalence_id(0x00A0_0F11), 0xa011);
        assert_eq!(equivalence_id(0x0083_0F10), 0x8310);
        let bytes17 = bytes(0x00810f81, 0x0810810e, FAMILY_17H);
        assert!(Patch::parse(&bytes17, 0x00810f81).is_ok());
        assert!(matches!(
            Patch::parse(&bytes17, 0x00810f80),
            Err(Error::ProcessorRevision { .. })
        ));
        assert!(matches!(
            Patch::parse(&bytes17[..FAMILY_17H - 1], 0x00810f81),
            Err(Error::Format)
        ));
        let mut chipset = bytes17.clone();
        chipset[16] = 1;
        assert!(matches!(
            Patch::parse(&chipset, 0x00810f81),
            Err(Error::Format)
        ));

        let rome = bytes(0x0083_0F10, 0x0830_107d, FAMILY_17H);
        let milan_b1 = bytes(0x00A0_0F11, 0x0a00_11db, MILAN);
        let milan_b2 = bytes(0x00A0_0F12, 0x0a00_1244, MILAN);
        let mut milan_chipset = bytes(0x00A0_0F12, 0x0a00_1245, MILAN);
        milan_chipset[0x1b] = 1;
        let short = bytes(0x00A0_0F12, 0x0a00_1246, FAMILY_17H);
        let all = [
            &rome[..],
            &short[..],
            &milan_chipset[..],
            &milan_b1[..],
            &milan_b2[..],
        ];
        assert_eq!(select(0x00A0_0F12, all).unwrap().revision(), 0x0a00_1244);
        assert!(select(0x00A0_0F10, all).is_none());
        assert_eq!(select(0x0083_0F10, all).unwrap().revision(), 0x0830_107d);
    }
    #[test]
    fn loader_has_no_downgrade_retry_or_preflight_writes() {
        let bytes = bytes(0x00810f81, 0x0810810e, FAMILY_17H);
        let patch = Patch::parse(&bytes, 0x00810f81).unwrap();
        let cpu = cpu(1, patch.revision());
        // Mock transport only: the numeric address is never dereferenced.
        let apply =
            |address, signature| unsafe { patch.apply(&cpu, address, signature, Load::default()) };
        assert_eq!(
            apply(0x2070000, 0x00810f81),
            Ok(Outcome::Installed(patch.revision()))
        );
        assert_eq!(writes(&cpu), [PATCH_LOADER.index()]);
        cpu.borrow_mut()
            .msrs
            .values
            .insert(PATCH_LEVEL.index(), u64::from(patch.revision() + 1));
        assert_eq!(
            apply(0x2070000, 0x00810f81),
            Ok(Outcome::AlreadyCurrent(patch.revision() + 1))
        );
        assert!(writes(&cpu).is_empty());
        cpu.borrow_mut().msrs.values.insert(PATCH_LEVEL.index(), 0);
        cpu.borrow_mut().loaded = 0;
        assert_eq!(
            apply(0x2070000, 0x00810f81),
            Err(Error::Rejected {
                expected: patch.revision(),
                actual: 0
            })
        );
        assert_eq!(writes(&cpu), [PATCH_LOADER.index()]);
        for address in [0, 0x2070001, (1 << 32) - 16, u64::MAX - 15] {
            assert_eq!(apply(address, 0x00810f81), Err(Error::Address));
        }
        assert_eq!(apply(0x2070000, 0x00810f80), Err(Error::CpuIdentity));
        assert!(writes(&cpu).is_empty());
    }
    #[test]
    fn naples_b1_quirks_and_the_s3_address() {
        let bytes = bytes(ZP_B1, 0x0800_1129, FAMILY_17H);
        let patch = Patch::parse(&bytes, ZP_B1).unwrap();
        let cpu = cpu(0x0800_1100, 0x0800_1129);
        fn flush() {}
        let load = Load {
            record_for_s3: true,
            writeback: flush,
        };
        let outcome = unsafe { patch.apply(&cpu, 0x2070000, ZP_B1, load) };
        assert_eq!(outcome, Ok(Outcome::Installed(0x0800_1129)));
        assert_eq!(
            writes(&cpu),
            [
                LS_CFG2.index(),
                PATCH_ADDR_S3.index(),
                PATCH_ADDR_S3.index(),
                PATCH_LOADER.index(),
                LS_CFG2.index(),
                C001_100B.index(),
            ]
        );
        let msrs = cpu.into_inner().msrs;
        assert_eq!(msrs.get(LS_CFG2.index()), 0);
        assert_eq!(msrs.get(PATCH_ADDR_S3.index()), 0x2070000);
        assert_eq!(msrs.get(C001_100B.index()), 1);

        // Installed by the PSP already: no load, the same bookkeeping.
        let cpu = self::cpu(0x0800_1129, 0x0800_1129);
        let outcome = unsafe { patch.apply(&cpu, 0x2070000, ZP_B1, load) };
        assert_eq!(outcome, Ok(Outcome::AlreadyCurrent(0x0800_1129)));
        assert_eq!(
            writes(&cpu),
            [
                PATCH_ADDR_S3.index(),
                PATCH_ADDR_S3.index(),
                C001_100B.index()
            ]
        );
        // A newer installed patch: this image isn't registered for S3.
        let cpu = self::cpu(0x0800_1130, 0x0800_1130);
        let outcome = unsafe { patch.apply(&cpu, 0x2070000, ZP_B1, load) };
        assert_eq!(outcome, Ok(Outcome::AlreadyCurrent(0x0800_1130)));
        assert_eq!(writes(&cpu), [C001_100B.index()]);
    }
}
