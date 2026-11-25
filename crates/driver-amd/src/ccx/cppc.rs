// SPDX-License-Identifier: MIT OR Apache-2.0
//! `_CPC` of the CPUs with CPPC in MSRs (Zen 6): the vendor's descriptor
//! table and the per-thread package the OS's CPPC driver (Linux
//! `amd-pstate`) needs.
//!
//! [`ROWS`] is the 21-row table AmdCcxZen6Dxe hands to AmdAcpiDxe's
//! `_CPC` builder, generated from the donors' bytes (Dell PR9XJ 1.0.8
//! AmdCcxZen6Dxe `0x25500`, GIGABYTE MD25-CG0 F01 `0x15410`; identical;
//! venice `tools/runtime_tables.py` → `reports/research/RUNTIME/cppc_cpc.json`).
//! Registers are FFixedHW MSRs (`GAS` space 7Fh, access size 4), the
//! same for every thread: CPPC_CAP1 `C001_02B0`, CPPC_ENABLE `…2B1`,
//! CPPC_CAP2 `…2B2`, CPPC_REQ `…2B3`, CPPC_STATUS `…2B4`, MPERF `E7h`,
//! APERF `E8h`.
//!
//! The builder (MD25 AmdAcpiDxe `AmdCreateCppcAcpiTables`, decompiler)
//! keeps the rows whose minimum revision is at most the SMU's `_CPC`
//! revision, puts a null register (an all-zero system-memory `GAS`) or
//! 0 where the SMU's CppcGetInfo mask says a row is not implemented, and
//! builds nothing when that answer says CPPC is off
//! ([`crate::smu::v14::msg::cppc_info`]). Its integer rows come from
//! callbacks: AutonomousSelectionEnable from a platform query whose
//! policy was not traced (fstart: 0, OS-directed), LowestFrequency and
//! NominalFrequency from SMU messages 40h and 3Ah.

extern crate alloc;

use fstart_acpi::gas::{AccessSize, AddressSpace, GAS};
use fstart_acpi::processor::CpcEntry;
use heapless::Vec;

use crate::smu::CppcInfo;

/// What a row describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    /// Bits `bit_offset..bit_offset + bit_width` of MSR `msr` (FFixedHW).
    Msr {
        msr: u32,
        bit_width: u8,
        bit_offset: u8,
    },
    /// No register (the vendor's all-zero descriptor).
    Null,
    /// An integer the firmware computes.
    Integer(Value),
}

/// The integers of the integer rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value {
    AutonomousSelectionEnable,
    LowestFrequency,
    NominalFrequency,
}

/// A `_CPC` row and the `_CPC` revision it needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpcRow {
    pub row: Row,
    pub min_revision: u8,
}

const fn msr(msr: u32, bit_width: u8, bit_offset: u8) -> CpcRow {
    CpcRow {
        row: Row::Msr {
            msr,
            bit_width,
            bit_offset,
        },
        min_revision: 2,
    }
}
const NULL: CpcRow = CpcRow {
    row: Row::Null,
    min_revision: 2,
};
const fn integer(value: Value, min_revision: u8) -> CpcRow {
    CpcRow {
        row: Row::Integer(value),
        min_revision,
    }
}

// The CPPC MSRs' indices, from their typed definitions in `ccx::msr`.
const CPPC_CAP1: u32 = super::msr::CPPC_CAP1.index();
const CPPC_ENABLE: u32 = super::msr::CPPC_ENABLE.index();
const CPPC_CAP2: u32 = super::msr::CPPC_CAP2.index();
const CPPC_REQ: u32 = super::msr::CPPC_REQUEST.index();
const CPPC_STATUS: u32 = super::msr::CPPC_STATUS.index();
pub const MPERF: u32 = 0xe7;
pub const APERF: u32 = 0xe8;

/// The vendor's table, in `_CPC` order (ACPI 6.5 Table 8.24).
pub const ROWS: [CpcRow; 21] = [
    // HighestPerformance, NominalPerformance, LowestNonlinearPerformance,
    // LowestPerformance: CPPC_CAP1 bytes 3..0.
    msr(CPPC_CAP1, 8, 24),
    msr(CPPC_CAP1, 8, 16),
    msr(CPPC_CAP1, 8, 8),
    msr(CPPC_CAP1, 8, 0),
    // GuaranteedPerformanceRegister.
    msr(CPPC_CAP2, 8, 8),
    // DesiredPerformance, MinimumPerformance, MaximumPerformance.
    msr(CPPC_REQ, 8, 16),
    msr(CPPC_REQ, 8, 8),
    msr(CPPC_REQ, 8, 0),
    // PerformanceReductionTolerance, TimeWindow, CounterWraparoundTime.
    NULL,
    NULL,
    NULL,
    // Reference and delivered performance counters.
    msr(MPERF, 64, 0),
    msr(APERF, 64, 0),
    // PerformanceLimited.
    msr(CPPC_STATUS, 2, 0),
    // CPPCEnable.
    msr(CPPC_ENABLE, 1, 0),
    integer(Value::AutonomousSelectionEnable, 2),
    // AutonomousActivityWindow.
    NULL,
    // EnergyPerformancePreference: CPPC_REQ byte 3.
    msr(CPPC_REQ, 8, 24),
    // ReferencePerformance.
    NULL,
    integer(Value::LowestFrequency, 3),
    integer(Value::NominalFrequency, 3),
];

/// The values of the integer rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Integers {
    pub autonomous_selection: u32,
    pub lowest_mhz: u32,
    pub nominal_mhz: u32,
}

/// The `_CPC` entries (after NumEntries and Revision) as the vendor's
/// builder makes them from `info` ([`CppcInfo::revision`] is the package's
/// revision); empty when CPPC is off.
pub fn cpc_entries(info: &CppcInfo, integers: &Integers) -> Vec<CpcEntry, 21> {
    if !info.enabled() {
        return Vec::new();
    }
    ROWS.iter()
        .enumerate()
        .filter(|(_, row)| row.min_revision <= info.revision)
        .map(|(k, row)| {
            let implemented = info.implements(k as u8);
            match row.row {
                Row::Msr { .. } | Row::Null if !implemented => CpcEntry::none(),
                Row::Msr {
                    msr,
                    bit_width,
                    bit_offset,
                } => CpcEntry::Register(GAS::new(
                    AddressSpace::FunctionalFixedHardware,
                    bit_width,
                    bit_offset,
                    AccessSize::QwordAccess,
                    msr.into(),
                )),
                Row::Null => CpcEntry::none(),
                Row::Integer(_) if !implemented => CpcEntry::Integer(0),
                Row::Integer(Value::AutonomousSelectionEnable) => {
                    CpcEntry::Integer(integers.autonomous_selection)
                }
                Row::Integer(Value::LowestFrequency) => CpcEntry::Integer(integers.lowest_mhz),
                Row::Integer(Value::NominalFrequency) => CpcEntry::Integer(integers.nominal_mhz),
            }
        })
        .collect()
}

/// `Name(_CPC, ...)` for a thread, or nothing when CPPC is off.
pub fn cpc_aml(info: &CppcInfo, integers: &Integers) -> alloc::vec::Vec<u8> {
    let entries = cpc_entries(info, integers);
    if entries.is_empty() {
        return alloc::vec::Vec::new();
    }
    fstart_acpi::processor::cpc_aml(info.revision, &entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FREQUENCIES: Integers = Integers {
        autonomous_selection: 0,
        lowest_mhz: 400,
        nominal_mhz: 2400,
    };

    /// The donors' raw rows (`cppc_cpc.json`): kind, space, width, offset,
    /// access size, address, then the minimum revision.
    #[test]
    fn rows_match_the_donor_bytes() {
        let raw: [&str; 21] = [
            "0a7f081804b00201c000000000000000000000000002",
            "0a7f081004b00201c000000000000000000000000002",
            "0a7f080804b00201c000000000000000000000000002",
            "0a7f080004b00201c000000000000000000000000002",
            "0a7f080804b20201c000000000000000000000000002",
            "0a7f081004b30201c000000000000000000000000002",
            "0a7f080804b30201c000000000000000000000000002",
            "0a7f080004b30201c000000000000000000000000002",
            "0a000000000000000000000000000000000000000002",
            "0a000000000000000000000000000000000000000002",
            "0a000000000000000000000000000000000000000002",
            "0a7f400004e700000000000000000000000000000002",
            "0a7f400004e800000000000000000000000000000002",
            "0a7f020004b40201c000000000000000000000000002",
            "0a7f010004b10201c000000000000000000000000002",
            "00",
            "0a000000000000000000000000000000000000000002",
            "0a7f081804b30201c000000000000000000000000002",
            "0a000000000000000000000000000000000000000002",
            "00",
            "00",
        ];
        let byte = |s: &str, i: usize| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap();
        for (row, raw) in ROWS.iter().zip(raw) {
            match row.row {
                Row::Msr {
                    msr,
                    bit_width,
                    bit_offset,
                } => {
                    assert_eq!(
                        (byte(raw, 1), byte(raw, 2), byte(raw, 3), byte(raw, 4)),
                        (0x7f, bit_width, bit_offset, 4)
                    );
                    let address =
                        (0..8).fold(0u64, |a, i| a | u64::from(byte(raw, 5 + i)) << (8 * i));
                    assert_eq!(address, msr.into());
                    assert_eq!(byte(raw, 21), row.min_revision);
                }
                Row::Null => assert!(raw[2..42].bytes().all(|b| b == b'0')),
                Row::Integer(_) => assert_eq!(raw, "00"),
            }
        }
    }

    #[test]
    fn the_builder_filters_by_revision_and_mask() {
        // Revision 3, every register but 16 (the autonomous window).
        let info = CppcInfo::from_answer((0x1f_ffff & !(1 << 16)) << 2 | 3);
        let e = cpc_entries(&info, &FREQUENCIES);
        assert_eq!(e.len(), 21);
        assert!(
            matches!(e[0], CpcEntry::Register(g) if g.address.get() == 0xc001_02b0 && g.register_bit_offset == 24)
        );
        assert!(
            matches!(e[12], CpcEntry::Register(g) if g.address.get() == 0xe8 && g.register_bit_width == 64)
        );
        assert!(matches!(e[16], CpcEntry::Register(g) if g.address.get() == 0));
        assert!(matches!(e[19], CpcEntry::Integer(400)));
        assert!(matches!(e[20], CpcEntry::Integer(2400)));
        let aml = cpc_aml(&info, &FREQUENCIES);
        assert_eq!(&aml[..5], b"\x08_CPC");
        // 23 elements, revision 3; FFH descriptors of 64-bit access.
        assert!(aml.windows(5).any(|w| w == [0x17, 0x0a, 0x17, 0x0a, 0x03]));
        assert_eq!(
            aml.windows(4).filter(|w| *w == [0x82, 12, 0, 0x7f]).count(),
            13
        );

        // Revision 2: no frequencies, 19 entries.
        let info = CppcInfo::from_answer(0x1f_ffff << 2 | 2);
        assert_eq!(cpc_entries(&info, &FREQUENCIES).len(), 19);
        // A frequency the SMU does not implement is 0.
        let info = CppcInfo::from_answer((0x1f_ffff & !(1 << 19)) << 2 | 3);
        assert!(matches!(
            cpc_entries(&info, &FREQUENCIES)[19],
            CpcEntry::Integer(0)
        ));
        // CPPC off (bit 16 of the answer): nothing.
        let off = CppcInfo::from_answer((0x1f_ffff & !(1 << 14)) << 2 | 3);
        assert!(cpc_entries(&off, &FREQUENCIES).is_empty());
        assert!(cpc_aml(&off, &FREQUENCIES).is_empty());
    }
}
