//! CPPC through the SMU: the PCC shared memory the SMU serves, its PCCT
//! subspace and the per-thread `_CPC` (AmdAcpiDxe `0x27cc`, template
//! `0x4f50`; CCX PCCT service in AmdCcxVhDxe / AmdCcxZen3Dxe, same code as
//! am4-re ACPI_DXE §3).
//!
//! The region is 64 KiB, 64 KiB aligned, uncached on every thread; offset
//! 0 holds `PCC\0`-style signature `0x5043_4300` (subspace 0) and thread
//! `n` of the PPTable CPPC map (`pptable::cppc_map` order) owns
//! `0x100 · n`.

use fstart_acpi::gas::{AccessSize, AddressSpace, GAS};
use fstart_acpi::pcct::GenericSubspace;
use fstart_acpi::processor::CpcEntry;

use super::{CppcInfo, cppc_doorbell};

pub const REGION_SIZE: u64 = 0x1_0000;
/// Offset 0 of the region: PCC signature of subspace 0.
pub const SIGNATURE: u32 = 0x5043_4300;
/// Bytes of PCC registers per thread.
pub const SLOT: u64 = 0x100;

/// `_CPC` registers 0–18: PCC offsets, except the reference and delivered
/// counters (FFH MSRs 0xE7/0xE8).
const REGISTERS: [Register; 19] = {
    use Register::{Ffh, Pcc};
    [
        Pcc(0x00),
        Pcc(0x04),
        Pcc(0x08),
        Pcc(0x0C),
        Pcc(0x10),
        Pcc(0x14),
        Pcc(0x18),
        Pcc(0x1C),
        Pcc(0x20),
        Pcc(0x24),
        Pcc(0x28),
        Ffh(0xE7),
        Ffh(0xE8),
        Pcc(0x40),
        Pcc(0x44),
        Pcc(0x48),
        Pcc(0x4C),
        Pcc(0x50),
        Pcc(0x54),
    ]
};

#[derive(Clone, Copy)]
enum Register {
    Pcc(u64),
    Ffh(u64),
}

/// The 21 `_CPC` entries of PPTable thread `slot`. Unimplemented
/// registers are null, unimplemented frequencies 0 (as the donor).
pub fn cpc_entries(
    info: &CppcInfo,
    slot: u32,
    lowest_mhz: u32,
    nominal_mhz: u32,
) -> [CpcEntry; 21] {
    core::array::from_fn(|k| {
        let implemented = info.implements(k as u8);
        match (k, REGISTERS.get(k)) {
            (_, Some(_)) if !implemented => CpcEntry::none(),
            (_, Some(Register::Pcc(offset))) => CpcEntry::Register(GAS::new(
                AddressSpace::PlatformCommunicationsChannel,
                32,
                0,
                AccessSize::Undefined,
                offset + SLOT * u64::from(slot),
            )),
            (_, Some(Register::Ffh(msr))) => CpcEntry::Register(GAS::new(
                AddressSpace::FunctionalFixedHardware,
                64,
                0,
                AccessSize::QwordAccess,
                *msr,
            )),
            (19, None) => CpcEntry::Integer(if implemented { lowest_mhz } else { 0 }),
            _ => CpcEntry::Integer(if implemented { nominal_mhz } else { 0 }),
        }
    })
}

/// `_CPC` entries (after NumEntries and Revision) for `revision`: ACPI
/// revision 2 has 19 (no frequencies), revision 3 has 21. AmdAcpiDxe
/// always emits 21 with the SMU's revision, so a revision-2 SMU (the bench
/// Rome) gets a package Linux rejects ("Mismatched _CPC revision and
/// num_ent"); fstart emits the spec-valid count.
pub const fn cpc_entry_count(revision: u8) -> usize {
    if revision >= 3 { 21 } else { 19 }
}

/// The PCCT subspace for the region at `base` (doorbell in the SMU MMIO).
pub fn pcct_subspace(base: u64, smu_bar: u64) -> GenericSubspace {
    GenericSubspace {
        base,
        length: REGION_SIZE,
        doorbell: GAS::new(
            AddressSpace::SystemMemory,
            64,
            0,
            AccessSize::QwordAccess,
            cppc_doorbell(smu_bar),
        ),
        doorbell_preserve: 0xFFFF_FFFF_0000_0000,
        doorbell_write: 1,
        nominal_latency_us: 4000,
        max_periodic_access_rate: 60000,
        min_request_turnaround_us: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_follow_the_supported_mask() {
        // Revision 3, registers 0–15 and 17–20 (no autonomous window).
        let info = CppcInfo::from_answer((0x1F_FFFF & !(1 << 16)) << 2 | 3);
        let e = cpc_entries(&info, 2, 1500, 2500);
        let reg = |i: usize| match e[i] {
            CpcEntry::Register(g) => (g.address_space_id as u8, g.address.get()),
            CpcEntry::Integer(_) => panic!("register expected"),
        };
        assert_eq!(reg(0), (0x0A, 0x200));
        assert_eq!(reg(14), (0x0A, 0x244));
        assert_eq!(reg(12), (0x7F, 0xE8));
        assert_eq!(reg(16), (0, 0), "not implemented: null");
        assert!(matches!(e[19], CpcEntry::Integer(1500)));
        assert!(matches!(e[20], CpcEntry::Integer(2500)));
        assert_eq!((cpc_entry_count(2), cpc_entry_count(3)), (19, 21));
    }
}
