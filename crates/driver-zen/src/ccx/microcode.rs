//! Microcode patches (CCX_SP3 §3.8, instruction-checked).
//!
//! Patches are BIOS directory type 0x66 entries, one patch each: 0xC80
//! bytes on Naples and Rome, 0x15C0 on Milan. A patch matches when its
//! processor revision ID (header +0x18) equals the CPU's equivalence ID
//! `(eax >> 8 & 0xff00) | (eax & 0xff)` of CPUID Fn0000_0001_EAX. The donors
//! also compare NB/SB device IDs and revisions against bus 0 (zero = any);
//! all donor patches have zero there, and a patch that names a chipset is
//! rejected here (fail closed) rather than scanning bus 0.
//!
//! The donors load in DXE on the BSP and then on each AP's primary thread;
//! PEI runs at the ROM patch level. Load on the primary thread of every core
//! (SMT siblings share the patch).

use fstart_arch::x86::cpu::msr::{MsrAccess, PATCH_LEVEL};
use tock_registers::interfaces::Readable;
use zerocopy::little_endian::{U16, U32};
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

use crate::soc::{Generation, Revision};

/// PATCH_LOADER: writing a patch address loads it.
pub const PATCH_LOADER: u32 = 0xc001_0020;
/// The loaded patch's address, kept for the PSP's S3 resume reload.
pub const PATCH_ADDR_S3: u32 = 0xc001_100c;
const LS_CFG2: u32 = 0xc001_102d;
const C001_100B: u32 = 0xc001_100b;

/// Patch size of `generation`.
pub const fn patch_size(generation: Generation) -> usize {
    match generation {
        Generation::Naples | Generation::Rome => 0xc80,
        Generation::Milan => 0x15c0,
    }
}

/// The equivalence ID of CPUID Fn0000_0001_EAX.
pub const fn equivalence_id(signature: u32) -> u16 {
    (((signature >> 8) & 0xff00) | (signature & 0xff)) as u16
}

/// The patch header (Linux `microcode_header_amd`).
#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct Header {
    date: U32,
    patch_id: U32,
    data_id: U16,
    data_len: u8,
    init_flag: u8,
    checksum: U32,
    nb_dev_id: U32,
    sb_dev_id: U32,
    processor_rev_id: U16,
    nb_rev_id: u8,
    sb_rev_id: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicrocodeError {
    /// Not one patch of the generation's size.
    Size(usize),
    /// The patch level did not change to the patch's ID.
    NotLoaded { expected: u32, level: u32 },
}

/// One patch, in memory the CPU can read at its (identity-mapped) address.
#[derive(Clone, Copy)]
pub struct Patch<'a> {
    bytes: &'a [u8],
    header: &'a Header,
}

impl<'a> Patch<'a> {
    pub fn parse(generation: Generation, bytes: &'a [u8]) -> Result<Self, MicrocodeError> {
        if bytes.len() != patch_size(generation) {
            return Err(MicrocodeError::Size(bytes.len()));
        }
        let (header, _) =
            Header::ref_from_prefix(bytes).map_err(|_| MicrocodeError::Size(bytes.len()))?;
        Ok(Self { bytes, header })
    }

    pub fn patch_id(&self) -> u32 {
        self.header.patch_id.get()
    }

    pub fn equivalence_id(&self) -> u16 {
        self.header.processor_rev_id.get()
    }

    /// The patch names no northbridge/southbridge.
    fn any_chipset(&self) -> bool {
        let h = self.header;
        h.nb_dev_id.get() == 0 && h.sb_dev_id.get() == 0 && h.nb_rev_id == 0 && h.sb_rev_id == 0
    }

    pub fn address(&self) -> u64 {
        self.bytes.as_ptr() as u64
    }
}

/// The first of `candidates` (BIOS directory 0x66 entries) for the CPU.
pub fn select<'a>(
    generation: Generation,
    signature: u32,
    candidates: impl IntoIterator<Item = &'a [u8]>,
) -> Option<Patch<'a>> {
    let id = equivalence_id(signature);
    candidates
        .into_iter()
        .filter_map(|b| Patch::parse(generation, b).ok())
        .find(|p| p.equivalence_id() == id && p.any_chipset())
}

/// Load `patch` on the executing core. `wbinvd` is called where Naples
/// A0–B1 need the caches written back.
pub fn load<M: MsrAccess + ?Sized>(
    cpu: &M,
    revision: Revision,
    patch: &Patch<'_>,
    wbinvd: impl Fn(),
) -> Result<(), MicrocodeError> {
    let early_zp = matches!(revision, Revision::ZpA0 | Revision::ZpB0 | Revision::ZpB1);
    let ls_cfg2_bit7 = 1 << 7;
    if early_zp {
        cpu.write(LS_CFG2, cpu.read(LS_CFG2) | ls_cfg2_bit7);
    }
    let addr = patch.address();
    cpu.write(PATCH_ADDR_S3, 0);
    cpu.write(PATCH_ADDR_S3, addr);
    cpu.write(PATCH_LOADER, addr);
    if early_zp {
        wbinvd();
        cpu.write(LS_CFG2, cpu.read(LS_CFG2) & !ls_cfg2_bit7);
    }
    let level = cpu.reg(PATCH_LEVEL).get() as u32;
    if level != patch.patch_id() {
        return Err(MicrocodeError::NotLoaded {
            expected: patch.patch_id(),
            level,
        });
    }
    // ZP B1 with a patch newer than 0x8001117 (sp4r2 CCX_ZP §3.7).
    if revision == Revision::ZpB1 && level > 0x0800_1117 {
        cpu.write(C001_100B, cpu.read(C001_100B) | 1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_arch::x86::cpu::msr::testing::{MsrModel, MsrSpace};
    use std::cell::RefCell;

    fn patch(generation: Generation, id: u32, equiv: u16) -> std::vec::Vec<u8> {
        let mut b = vec![0u8; patch_size(generation)];
        b[4..8].copy_from_slice(&id.to_le_bytes());
        b[0x18..0x1a].copy_from_slice(&equiv.to_le_bytes());
        b
    }

    /// Loads take effect: PATCH_LOADER sets the patch level.
    struct Cpu(MsrSpace, u32);
    impl MsrModel for Cpu {
        fn read(&mut self, index: u32) -> u64 {
            self.0.read(index)
        }
        fn write(&mut self, index: u32, value: u64) {
            if index == PATCH_LOADER {
                self.0.write(0x8b, self.1 as u64);
            }
            self.0.write(index, value)
        }
    }

    #[test]
    fn selection() {
        assert_eq!(equivalence_id(0x00A0_0F11), 0xa011);
        assert_eq!(equivalence_id(0x0083_0F10), 0x8310);
        let rome = patch(Generation::Rome, 0x0830_107d, 0x8310);
        let milan_b1 = patch(Generation::Milan, 0x0a00_11db, 0xa011);
        let milan_b2 = patch(Generation::Milan, 0x0a00_1244, 0xa012);
        let mut chipset = patch(Generation::Milan, 0x0a00_1245, 0xa012);
        chipset[0x10] = 1;
        let all = [&rome[..], &chipset[..], &milan_b1[..], &milan_b2[..]];
        let p = select(Generation::Milan, 0x00A0_0F12, all).unwrap();
        assert_eq!(
            p.patch_id(),
            0x0a00_1244,
            "wrong size and chipset patches skipped"
        );
        assert!(select(Generation::Milan, 0x00A0_0F10, all).is_none());
        assert_eq!(
            select(Generation::Rome, 0x0083_0F10, all)
                .unwrap()
                .patch_id(),
            0x0830_107d
        );
    }

    #[test]
    fn naples_b1_quirks() {
        let bytes = patch(Generation::Naples, 0x0800_1129, 0x8011);
        let p = Patch::parse(Generation::Naples, &bytes).unwrap();
        let cpu = RefCell::new(Cpu(MsrSpace::default(), 0x0800_1129));
        let flushed = core::cell::Cell::new(false);
        load(&cpu, Revision::ZpB1, &p, || flushed.set(true)).unwrap();
        let cpu = cpu.into_inner().0;
        let order: std::vec::Vec<_> = cpu.writes.iter().map(|w| w.0).collect();
        assert_eq!(
            order,
            [
                LS_CFG2,
                PATCH_ADDR_S3,
                PATCH_ADDR_S3,
                0x8b,
                PATCH_LOADER,
                LS_CFG2,
                C001_100B
            ]
        );
        assert!(flushed.get());
        assert_eq!(cpu.get(LS_CFG2), 0);
        assert_eq!(cpu.get(PATCH_ADDR_S3), p.address());

        // Rome: plain load; a refused patch is an error.
        let bytes = patch(Generation::Rome, 0x0830_107d, 0x8310);
        let p = Patch::parse(Generation::Rome, &bytes).unwrap();
        let cpu = RefCell::new(Cpu(MsrSpace::default(), 0x0830_1052));
        assert_eq!(
            load(&cpu, Revision::SspB0, &p, || unreachable!()),
            Err(MicrocodeError::NotLoaded {
                expected: 0x0830_107d,
                level: 0x0830_1052
            })
        );
    }
}
