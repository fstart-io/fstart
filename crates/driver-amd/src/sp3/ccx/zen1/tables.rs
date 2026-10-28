//! Naples (ZP, Zen) MSR table (CCX_SP3 §3.3), generated: do not edit.
//!
//! `.venv/bin/python -m tools.gen_ccx` from `reports/ccx-contracts.json`;
//! donor `naples-s8026-v202`, AmdCcxZenZpPei / AmdCcxZenZpDxe
//! (69000fc4-15e6-45cf-a6d4-73337a9b5958 / 4a1a67d0-e77a-4f0a-af1d-cc412be93dc9). Every record is
//! `observed-bytes` in the PEI and the DXE copy; comments give both VAs.
//!
//! Dropped (no SP3 CPU can match):
//! - zp.table.s1.ffdc4756 TW_CFG clear 0x200000000000 set 0x200000000000: platform bit 0 (AM4 package)

use crate::sp3::ccx::logical::zp::*;
use crate::sp3::ccx::table::Record;
use crate::sp3::ccx::topology::Scope;

/// Phase-1 (cold boot) records in dispatch order; phase 3 (S3) is empty.
#[rustfmt::skip]
pub static RECORDS: &[Record] = &[
    // APIC_BAR: PEI 0xffdc465c / DXE 0xa1e0
    Record::new(Scope::Thread, 0x1b, 0x800, 0x800),
    // HWCR: PEI 0xffdc4672 / DXE 0xa1f6
    Record::new(Scope::Thread, 0xc001_0015, 0x8000000, 0x8000000),
    // CPUID_ExtFeatures: PEI 0xffdc4688 / DXE 0xa20c
    Record::new(Scope::Thread, 0xc001_1005, 0x1040000000000, 0x0),
    // MCA_CTL_MASK_IF: PEI 0xffdc46a4 / DXE 0xa230
    Record::new(Scope::Core, 0xc001_0401, 0x2000, 0x2000),
    // MCA_CTL_MASK_L2: PEI 0xffdc46ba / DXE 0xa246
    Record::new(Scope::Core, 0xc001_0402, 0x8, 0x8),
    // MCA_CTL_MASK_FP: PEI 0xffdc46d0 / DXE 0xa25c
    Record::new(Scope::Core, 0xc001_0406, 0x40, 0x40),
    // LS_CFG: PEI 0xffdc46e6 / DXE 0xa272
    Record::new(Scope::Core, 0xc001_1020, 0x10, 0x10).cpu(CPU, A0 | B0 | B1),
    // LS_CFG: PEI 0xffdc4702 / DXE 0xa28e
    Record::new(Scope::Core, 0xc001_1020, 0x200000000000000, 0x200000000000000).cpu(CPU, B1 | B2),
    // LS_CFG2: PEI 0xffdc471e / DXE 0xa2aa
    Record::new(Scope::Core, 0xc001_102d, 0x400000000, 0x400000000).cpu(CPU, B1),
    // LS_CFG2: PEI 0xffdc473a / DXE 0xa2c6
    Record::new(Scope::Core, 0xc001_102d, 0x400000020, 0x400000020).cpu(CPU, B2),
    // FP_CFG: PEI 0xffdc4770 / DXE 0xa2fc
    Record::new(Scope::Core, 0xc001_1028, 0x80000000000, 0x80000000000).cpu(CPU, A0),
    // FP_CFG: PEI 0xffdc478c / DXE 0xa318
    Record::new(Scope::Core, 0xc001_1028, 0x14, 0x14),
    // DE_CFG: PEI 0xffdc47a2 / DXE 0xa32e
    Record::new(Scope::Core, 0xc001_1029, 0x4000, 0x4000),
    // DE_CFG: PEI 0xffdc47b8 / DXE 0xa344
    Record::new(Scope::Core, 0xc001_1029, 0x400, 0x400).cpu(CPU, A0),
    // DE_CFG: PEI 0xffdc47d4 / DXE 0xa360
    Record::new(Scope::Core, 0xc001_1029, 0x22000, 0x22000),
    // DE_CFG: PEI 0xffdc47ea / DXE 0xa376
    Record::new(Scope::Core, 0xc001_1029, 0x10000000, 0x10000000).cpu(CPU, B2),
    // L2_CFG: PEI 0xffdc4806 / DXE 0xa392
    Record::new(Scope::Core, 0xc001_102a, 0x20, 0x20).cpu(CPU, A0),
    // L2_CFG: PEI 0xffdc4822 / DXE 0xa3ae
    Record::new(Scope::Core, 0xc001_102a, 0x280000000000000, 0x280000000000000).cpu(CPU, B0 | B1 | B2),
    // L2_CFG: PEI 0xffdc483e / DXE 0xa3ca
    Record::new(Scope::Core, 0xc001_102a, 0x400000000000000, 0x400000000000000).cpu(CPU, B2),
    // L2_CFG: PEI 0xffdc485a / DXE 0xa3e6
    Record::new(Scope::Core, 0xc001_102a, 0x8080, 0x8080),
    // UNKNOWN_C001_102C: PEI 0xffdc4870 / DXE 0xa3fc
    Record::new(Scope::Core, 0xc001_102c, 0x168000000000000, 0x168000000000000),
    // CH_L2_PF_CFG: PEI 0xffdc4886 / DXE 0xa412
    Record::new(Scope::Core, 0xc001_102b, 0x80000, 0x80000),
    // LS_CFG2: PEI 0xffdc489c / DXE 0xa428
    Record::new(Scope::Core, 0xc001_102d, 0x400000000, 0x400000000).cpu(CPU, A0),
    // LS_CFG2: PEI 0xffdc48b8 / DXE 0xa444
    Record::new(Scope::Core, 0xc001_102d, 0x1000000100000000, 0x1000000100000000),
    // BP_CFG: PEI 0xffdc48ce / DXE 0xa45a
    Record::new(Scope::Core, 0xc001_102e, 0x10000000, 0x10000000).cpu(CPU, A0 | B0),
    // BP_CFG: PEI 0xffdc48ea / DXE 0xa476
    Record::new(Scope::Core, 0xc001_102e, 0x1, 0x1).cpu(CPU, B2),
    // DPM_CFG: PEI 0xffdc4906 / DXE 0xa492
    Record::new(Scope::Core, 0xc001_1074, 0x1000000000000000, 0x0),
    // CH_L3_CFG1: PEI 0xffdc4924 / DXE 0xa4b0
    Record::new(Scope::Complex, 0xc001_1093, 0x5d9ffe, 0x480062).cpu(CPU, A0 | B0),
    // CH_L3_CFG1: PEI 0xffdc4940 / DXE 0xa4cc
    Record::new(Scope::Complex, 0xc001_1093, 0x5d9f7c, 0x480060).cpu(CPU, B1 | B2),
    // CH_L3_XI_CFG0: PEI 0xffdc495c / DXE 0xa4e8
    Record::new(Scope::Complex, 0xc001_1097, 0xffe, 0xff6),
];
