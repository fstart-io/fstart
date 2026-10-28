//! Rome (SSP, Zen 2) MSR table (CCX_SP3 §3.3), generated: do not edit.
//!
//! `.venv/bin/python -m tools.gen_ccx` from `reports/ccx-contracts.json`;
//! donor `rome-s8030-v404`, AmdCcxVhPei / AmdCcxVhDxe
//! (993e9acb-288c-4788-87ea-09f7a15fd765 / fc8cfd34-4d25-478c-83f0-0309c93ffba4). Every record is
//! `observed-bytes` in the PEI and the DXE copy; comments give both VAs.
//!
//! Dropped (no SP3 CPU can match):
//! - ssp.table.s1.ffdc8c68 CH_L2_CFG1 clear 0x100000000 set 0x100000000: cpu family 0x4 (Matisse (17h M7xh))

use crate::sp3::ccx::logical::ssp::*;
use crate::sp3::ccx::table::plat::ssp as plat;
use crate::sp3::ccx::table::{Platform, Record};
use crate::sp3::ccx::topology::Scope;

/// Phase-1 (cold boot) records in dispatch order; phase 3 (S3) is empty.
#[rustfmt::skip]
pub static RECORDS: &[Record] = &[
    // APIC_BAR: PEI 0xffdc89e0 / DXE 0xa840
    Record::new(Scope::Thread, 0x1b, 0x800, 0x800),
    // APIC_BAR: PEI 0xffdc89f6 / DXE 0xa856
    Record::new(Scope::Thread, 0x1b, 0x400, 0x400).platform(Platform::any(plat::X2APIC)),
    // MCA_CTL_MASK_PB: PEI 0xffdc8a10 / DXE 0xa870
    Record::new(Scope::Thread, 0xc001_0413, 0x2, 0x2).cpu(CPU, A0 | B0),
    // MCA_CTL_MASK_CS: PEI 0xffdc8a2c / DXE 0xa88c
    Record::new(Scope::Thread, 0xc001_0414, 0x2, 0x2).cpu(CPU, A0 | B0),
    // unnamed: PEI 0xffdc8a48 / DXE 0xa8a8
    Record::new(Scope::Thread, 0xc001_0415, 0x2, 0x2).cpu(CPU, A0 | B0),
    // CPUID_Features: PEI 0xffdc8a64 / DXE 0xa8c4
    Record::new(Scope::Thread, 0xc001_1004, 0x20000000000000, 0x0).platform(Platform::any(plat::XAPIC)),
    // HWCR: PEI 0xffdc8a7e / DXE 0xa8de
    Record::new(Scope::Thread, 0xc001_0015, 0x8000000, 0x8000000),
    // FEATURE_EXT_ID: PEI 0xffdc8a94 / DXE 0xa8f4
    Record::new(Scope::Thread, 0xc001_10dc, 0x80000000, 0x80000000).cpu(CPU, B0).platform(Platform::any(plat::IBS_31)),
    // MCA_CTL_MASK_LS: PEI 0xffdc8ab8 / DXE 0xa920
    Record::new(Scope::Core, 0xc001_0400, 0x600, 0x600),
    // MCA_CTL_MASK_IF: PEI 0xffdc8ace / DXE 0xa936
    Record::new(Scope::Core, 0xc001_0401, 0x2c00, 0x2c00),
    // MCA_CTL_MASK_L2: PEI 0xffdc8ae4 / DXE 0xa94c
    Record::new(Scope::Core, 0xc001_0402, 0x8, 0x8),
    // MCA_CTL_MASK_FP: PEI 0xffdc8afa / DXE 0xa962
    Record::new(Scope::Core, 0xc001_0406, 0x40, 0x40),
    // LS_CFG: PEI 0xffdc8b10 / DXE 0xa978
    Record::new(Scope::Core, 0xc001_1020, 0x1c0c000000000, 0x404000000000),
    // DC_CFG: PEI 0xffdc8b26 / DXE 0xa98e
    Record::new(Scope::Core, 0xc001_1022, 0x2000000, 0x2000000).core(CORE, B0),
    // TW_CFG: PEI 0xffdc8b42 / DXE 0xa9aa
    Record::new(Scope::Core, 0xc001_1023, 0x20, 0x20).core(CORE, B0),
    // FP_CFG: PEI 0xffdc8b5e / DXE 0xa9c6
    Record::new(Scope::Core, 0xc001_1028, 0x14, 0x14),
    // DE_CFG: PEI 0xffdc8b74 / DXE 0xa9dc
    Record::new(Scope::Core, 0xc001_1029, 0x4037e0310000000, 0x3000310000000),
    // DE_CFG: PEI 0xffdc8b8a / DXE 0xa9f2
    Record::new(Scope::Core, 0xc001_1029, 0x1000000000, 0x0).core(CORE, B0),
    // L2_CFG: PEI 0xffdc8ba6 / DXE 0xaa0e
    Record::new(Scope::Core, 0xc001_102a, 0x18080, 0x18080),
    // UNKNOWN_C001_102C: PEI 0xffdc8bbc / DXE 0xaa24
    Record::new(Scope::Core, 0xc001_102c, 0x308000000000000, 0x308000000000000),
    // LS_CFG2: PEI 0xffdc8bd2 / DXE 0xaa3a
    Record::new(Scope::Core, 0xc001_102d, 0x400000000, 0x400000000),
    // LS_CFG2: PEI 0xffdc8be8 / DXE 0xaa50
    Record::new(Scope::Core, 0xc001_102d, 0x10, 0x10).core(CORE, B0),
    // BP_CFG: PEI 0xffdc8c04 / DXE 0xaa6c
    Record::new(Scope::Core, 0xc001_102e, 0x2024000000000, 0x2024000000000),
    // BP_CFG: PEI 0xffdc8c1a / DXE 0xaa82
    Record::new(Scope::Core, 0xc001_102e, 0x200020020, 0x200020020).core(CORE, A0),
    // BP_CFG: PEI 0xffdc8c36 / DXE 0xaa9e
    Record::new(Scope::Core, 0xc001_102e, 0x10000000000000, 0x10000000000000).core(CORE, B0),
    // CH_L2_CFG1: PEI 0xffdc8c52 / DXE 0xaaba
    Record::new(Scope::Core, 0xc001_10e2, 0xfefa00082018, 0x2afa00082018),
    // UNKNOWN_C001_10E3: PEI 0xffdc8c84 / DXE 0xaaec
    Record::new(Scope::Core, 0xc001_10e3, 0x1, 0x1).core(CORE, B0),
    // DPM_CFG: PEI 0xffdc8ca0 / DXE 0xab08
    Record::new(Scope::Core, 0xc001_1074, 0x1000000000000000, 0x0),
    // CH_L3_CFG1: PEI 0xffdc8cb8 / DXE 0xab20
    Record::new(Scope::Complex, 0xc001_1093, 0xff5c71, 0x715870),
    // CH_L3_XI_CFG0: PEI 0xffdc8cce / DXE 0xab36
    Record::new(Scope::Complex, 0xc001_1097, 0x5ffe, 0x5dbe),
];
