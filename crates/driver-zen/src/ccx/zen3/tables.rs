//! Milan (GN, Zen 3) MSR table (CCX_SP3 §3.3), generated: do not edit.
//!
//! `.venv/bin/python -m tools.gen_ccx` from `reports/ccx-contracts.json`;
//! donor `milan-s8030-v404`, AmdCcxZen3Pei / AmdCcxZen3Dxe
//! (e947cbdb-858a-423e-95f6-019d1814de2a / b342bebb-41c3-40ab-aa70-e9f3742d1835). Every record is
//! `observed-bytes` in the PEI and the DXE copy; comments give both VAs.
//!
//! Dropped (no SP3 CPU can match):
//! - gn.table.s0.ffd0089e HWCR clear 0x100000000 set 0x100000000: cpu family 0x200 (Cezanne (19h M5xh))
//! - gn.table.s0.ffd008ba HWCR clear 0x100000000 set 0x100000000: cpu family 0x100 (Vermeer (19h M2xh))
//! - gn.table.s0.ffd00a76 FEATURE_EXT_ID clear 0x100 set 0x100: cpu family 0x400 (19h M3xh)
//! - gn.table.s0.ffd00aae FEATURE_EXT2_EAX clear 0x10 set 0x0: cpu family 0x200 (Cezanne (19h M5xh))
//! - gn.table.s1.ffd00b78 IC_CFG clear 0x38000200000180 set 0x100: cpu family 0x200 (Cezanne (19h M5xh))
//! - gn.table.s1.ffd00d0a BP_CFG clear 0x20 set 0x20: cpu family 0x200 (Cezanne (19h M5xh))
//! - gn.table.s1.ffd00dd8 CH_L2_AA_CFG clear 0xff0 set 0xff0: cpu family 0x100 (Vermeer (19h M2xh))

use crate::ccx::logical::gn::*;
use crate::ccx::table::plat::gn as plat;
use crate::ccx::table::{Platform, Record};
use crate::ccx::topology::Scope;

/// Phase-1 (cold boot) records in dispatch order; phase 3 (S3) is empty.
#[rustfmt::skip]
pub static RECORDS: &[Record] = &[
    // APIC_BAR: PEI 0xffd0083c / DXE 0xc9b0
    Record::new(Scope::Thread, 0x1b, 0x800, 0x800),
    // APIC_BAR: PEI 0xffd00852 / DXE 0xc9c6
    Record::new(Scope::Thread, 0x1b, 0x400, 0x400).platform(Platform::any(plat::X2APIC)),
    // HWCR: PEI 0xffd0086c / DXE 0xc9e0
    Record::new(Scope::Thread, 0xc001_0015, 0x8000000, 0x8000000),
    // HWCR: PEI 0xffd00882 / DXE 0xc9f6
    Record::new(Scope::Thread, 0xc001_0015, 0x100000000, 0x100000000).core(CORE, A0),
    // HWCR: PEI 0xffd008d6 / DXE 0xca4a
    Record::new(Scope::Thread, 0xc001_0015, 0x6000, 0x6000).cpu(CPU, A0 | B0 | B1 | B2),
    // MCA_CTL_MASK_LS: PEI 0xffd008f2 / DXE 0xca66
    Record::new(Scope::Thread, 0xc001_0400, 0x380000, 0x380000),
    // MCA_CTL_MASK_LS: PEI 0xffd00908 / DXE 0xca7c
    Record::new(Scope::Thread, 0xc001_0400, 0x1004, 0x1004).core(CORE, A0),
    // MCA_CTL_MASK_IF: PEI 0xffd00924 / DXE 0xca98
    Record::new(Scope::Thread, 0xc001_0401, 0x20800, 0x20800).core(CORE, A0),
    // MCA_CTL_MASK_IF: PEI 0xffd00940 / DXE 0xcab4
    Record::new(Scope::Thread, 0xc001_0401, 0x2400, 0x2400).cpu(CPU, A0 | B0 | B1 | B2),
    // MCA_CTL_MASK_L2: PEI 0xffd0095c / DXE 0xcad0
    Record::new(Scope::Thread, 0xc001_0402, 0x8, 0x8),
    // MCA_CTL_MASK_FP: PEI 0xffd00972 / DXE 0xcae6
    Record::new(Scope::Thread, 0xc001_0406, 0x40, 0x40),
    // MCODE_CTL: PEI 0xffd00988 / DXE 0xcafc
    Record::new(Scope::Thread, 0xc001_1000, 0xff000000, 0x33000000).cpu(CPU, B0 | B1 | B2),
    // CPUID_7_Features: PEI 0xffd009a4 / DXE 0xcb18
    Record::new(Scope::Thread, 0xc001_1002, 0x810, 0x0),
    // CPUID_7_Features: PEI 0xffd009ba / DXE 0xcb2e
    Record::new(Scope::Thread, 0xc001_1002, 0x200, 0x200).cpu(CPU, B0 | B1 | B2),
    // CPUID_Features: PEI 0xffd009d6 / DXE 0xcb4a
    Record::new(Scope::Thread, 0xc001_1004, 0x20000000000000, 0x0).platform(Platform::any(plat::XAPIC)),
    // CPUID_Features: PEI 0xffd009f0 / DXE 0xcb64
    Record::new(Scope::Thread, 0xc001_1004, 0x4000000000000000, 0x0).platform(Platform::any(plat::HIDE_RNG)),
    // CPUID_7_Features: PEI 0xffd00a0a / DXE 0xcb7e
    Record::new(Scope::Thread, 0xc001_1002, 0x40000, 0x0).platform(Platform::any(plat::HIDE_RNG)),
    // FEATURE_EXT_ID: PEI 0xffd00a24 / DXE 0xcb98
    Record::new(Scope::Thread, 0xc001_10dc, 0x400000, 0x0),
    // FEATURE_EXT_ID: PEI 0xffd00a3a / DXE 0xcbae
    Record::new(Scope::Thread, 0xc001_10dc, 0x100, 0x100).cpu(CPU, A0 | B0 | B1 | B2),
    // FEATURE_EXT_ID: PEI 0xffd00a56 / DXE 0xcbca
    Record::new(Scope::Thread, 0xc001_10dc, 0x80000000, 0x80000000).cpu(CPU, B0 | B1 | B2).platform(Platform::any(plat::IBS_31)),
    // FEATURE_EXT2_EAX: PEI 0xffd00a92 / DXE 0xcc06
    Record::new(Scope::Thread, 0xc001_10de, 0x10, 0x0).core(CORE, B0),
    // FEATURE_EXT2_EAX: PEI 0xffd00aca / DXE 0xcc3e
    Record::new(Scope::Thread, 0xc001_10de, 0x10, 0x0).cpu(CPU, B0 | B1 | B2),
    // STRUCT_EXT_FEAT_ID_EDX0_ECX0: PEI 0xffd00ae6 / DXE 0xcc5a
    Record::new(Scope::Thread, 0xc001_10df, 0x1000000000, 0x1000000000).cpu(CPU, B0 | B1 | B2),
    // LS_CFG: PEI 0xffd00b14 / DXE 0xcca0
    Record::new(Scope::Core, 0xc001_1020, 0x1c400000000000, 0x4400000000000),
    // LS_CFG: PEI 0xffd00b2a / DXE 0xccb6
    Record::new(Scope::Core, 0xc001_1020, 0x80000000000, 0x80000000000).core(CORE, B0 | B1 | B2),
    // IC_CFG: PEI 0xffd00b46 / DXE 0xccd2
    Record::new(Scope::Core, 0xc001_1021, 0x3e000000000000, 0x18000000000000),
    // IC_CFG: PEI 0xffd00b5c / DXE 0xcce8
    Record::new(Scope::Core, 0xc001_1021, 0x39000000000180, 0x19000000000080).core(CORE, A0),
    // IC_CFG: PEI 0xffd00b94 / DXE 0xcd20
    Record::new(Scope::Core, 0xc001_1021, 0x180, 0x100).cpu(CPU, A0),
    // DE_CFG: PEI 0xffd00bb0 / DXE 0xcd3c
    Record::new(Scope::Core, 0xc001_1029, 0x310000000, 0x310000000),
    // DE_CFG: PEI 0xffd00bc6 / DXE 0xcd52
    Record::new(Scope::Core, 0xc001_1029, 0x1800000000000000, 0x1800000000000000).core(CORE, A0),
    // DE_CFG: PEI 0xffd00be2 / DXE 0xcd6e
    Record::new(Scope::Core, 0xc001_1029, 0x1800000000000000, 0x0).core(CORE, B0),
    // DE_CFG: PEI 0xffd00bfe / DXE 0xcd8a
    Record::new(Scope::Core, 0xc001_1029, 0x1000000000000, 0x1000000000000).core(CORE, B1),
    // DE_CFG: PEI 0xffd00c1a / DXE 0xcda6
    Record::new(Scope::Core, 0xc001_1029, 0x1000000000000, 0x1000000000000).core(CORE, B2),
    // L2_CFG: PEI 0xffd00c36 / DXE 0xcdc2
    Record::new(Scope::Core, 0xc001_102a, 0x18080, 0x18080),
    // UNKNOWN_C001_102C: PEI 0xffd00c4c / DXE 0xcdd8
    Record::new(Scope::Core, 0xc001_102c, 0x400000000000000, 0x400000000000000).core(CORE, B0),
    // UNKNOWN_C001_102C: PEI 0xffd00c68 / DXE 0xcdf4
    Record::new(Scope::Core, 0xc001_102c, 0x400000000000000, 0x400000000000000).cpu(CPU, B0 | B1 | B2),
    // LS_CFG2: PEI 0xffd00c84 / DXE 0xce10
    Record::new(Scope::Core, 0xc001_102d, 0x200801800000000, 0x200800800000000).core(CORE, A0),
    // LS_CFG2: PEI 0xffd00ca0 / DXE 0xce2c
    Record::new(Scope::Core, 0xc001_102d, 0x218801800000000, 0x1800000000).core(CORE, B0 | B1 | B2),
    // BP_CFG: PEI 0xffd00cbc / DXE 0xce48
    Record::new(Scope::Core, 0xc001_102e, 0x1c, 0x0),
    // BP_CFG: PEI 0xffd00cd2 / DXE 0xce5e
    Record::new(Scope::Core, 0xc001_102e, 0x4022, 0x4022).core(CORE, A0),
    // BP_CFG: PEI 0xffd00cee / DXE 0xce7a
    Record::new(Scope::Core, 0xc001_102e, 0x4062, 0x40).core(CORE, B0),
    // BP_CFG: PEI 0xffd00d26 / DXE 0xceb2
    Record::new(Scope::Core, 0xc001_102e, 0x60, 0x60).cpu(CPU, B0 | B1 | B2),
    // CH_L2_CFG1: PEI 0xffd00d42 / DXE 0xcece
    Record::new(Scope::Core, 0xc001_10e2, 0x2000100080018, 0x2000000080018),
    // LS_CFG3: PEI 0xffd00d58 / DXE 0xcee4
    Record::new(Scope::Core, 0xc001_10e5, 0x5300000040002000, 0x5300000040002000),
    // LS_CFG3: PEI 0xffd00d6e / DXE 0xcefa
    Record::new(Scope::Core, 0xc001_10e5, 0x4100040004000100, 0x4000100).core(CORE, B0 | B1 | B2),
    // LS_CFG4: PEI 0xffd00d8a / DXE 0xcf16
    Record::new(Scope::Core, 0xc001_10e6, 0x1001, 0x1001).core(CORE, A0),
    // DC_CFG2: PEI 0xffd00da6 / DXE 0xcf32
    Record::new(Scope::Core, 0xc001_10e7, 0x88, 0x88),
    // DC_CFG2: PEI 0xffd00dbc / DXE 0xcf48
    Record::new(Scope::Core, 0xc001_10e7, 0x8, 0x0).core(CORE, B0 | B1 | B2),
    // CH_L2_AA_CFG: PEI 0xffd00df4 / DXE 0xcf80
    Record::new(Scope::Core, 0xc001_10e8, 0xff0, 0xff0).cpu(CPU, A0 | B0 | B1 | B2),
    // CH_L2_AA_PAIR_CFG0: PEI 0xffd00e10 / DXE 0xcf9c
    Record::new(Scope::Core, 0xc001_10e9, 0x1000000, 0x1000000),
    // CH_L2_AA_PAIR_CFG1: PEI 0xffd00e26 / DXE 0xcfb2
    Record::new(Scope::Core, 0xc001_10ea, 0xc000f0, 0x400000),
    // CH_L3_CFG1: PEI 0xffd007ec / DXE 0xc960
    Record::new(Scope::Complex, 0xc001_1093, 0xdc1f80, 0x501680),
    // CH_L3_XI_CFG0: PEI 0xffd00802 / DXE 0xc976
    Record::new(Scope::Complex, 0xc001_1097, 0x11c6, 0x1106),
    // CH_L3_XI_CFG0: PEI 0xffd00818 / DXE 0xc98c
    Record::new(Scope::Complex, 0xc001_1097, 0xe00, 0xc00).cpu(CPU, B0 | B1 | B2),
];
