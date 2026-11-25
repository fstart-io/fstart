// SPDX-License-Identifier: MIT OR Apache-2.0
//! WH (Venice; Zen 6 and Zen 6c) MSR table, generated: do not edit.
//!
//! The reset table of openSIL's CcxProgramTablesAtReset (time point 1; time
//! point 3, S3, is empty), decoded by `tools/ccx/gen_wh_tables.py` of the
//! venice analysis workspace (`reports/research/CCX/wh.rs`, with
//! `wh_reset_table.json`; only this header and the record type,
//! [`TestedRecord`] where it writes `Record`, differ from its output). Both
//! donors' SilVenicePei were decoded:
//!
//! - Dell PR9XJ 1.0.8 (VenicePI-SP7 1.0.0.0), PE sha256
//!   `6a838139f1b5ef20c2a28cd9a02f92e396e9237d8a6395cea8b6ca8dcc15d1b8`:
//!   the reference. Its time-point list is `f9ca48`, the thread, core and
//!   complex tables `f9e914`, `f9e90c`, `f9e904`; each record gives its
//!   address in the Dell image.
//! - GIGABYTE MD25-CG0 F01 (an older PI, `image_debug.bin`), PE sha256
//!   `e878d40a6ac609063aae78b47587aef3ad1be6d3319989ef712c3c4db6ad1fb8`:
//!   the same logical-ID table and the same non-MCA records. Its drift is
//!   in MCA records only (78 WH records against 67): the MCA_CONFIG clear
//!   masks of 21 non-core bank records are `0x185_0000_0200` /
//!   `0x181_0000_0200` (Dell also clears bit 33, TransparentLogging, which
//!   [`crate::ccx::mca::configure`] clears after the table anyway); it
//!   adds `MCA_CTL_MASK C001_041A..041C |= 0xE_3800` for HWID 0x1E1 banks
//!   (removed in the newer PI), and 8 `MCA_CTL_MASK C001_0407..040E |= 0x80`
//!   records guarded on logical revision 0x1 with model 8, which no WH
//!   revision row has (dead code).
//!
//! Only records that can apply to a WH SoC logical family (0x0f00_0000)
//! are kept. The engine is [`crate::ccx::table::apply_every_scope`]: the
//! vendor runs three tables (thread, core primary, complex primary) and,
//! unlike RS, a failing scope does not end the run. The SoC guards use the
//! WH logical IDs of [`LOGICAL`] ([`crate::ccx::wh::Revision::logical_id`]);
//! `.msr_test(msr, mask, value)` applies a record only if
//! `rdmsr(msr) & mask == value` (MCA banks picked by MCA_IPID). Platform
//! bits: [`crate::ccx::table::plat::wh`] (0x10 x2APIC, 0x20 not x2APIC).

use crate::ccx::table::{Platform, TestedRecord};
use crate::ccx::topology::Scope;

/// Phase-1 (cold boot) records in dispatch order.
#[rustfmt::skip]
pub static RECORDS: &[TestedRecord] = &[
    // 00f9cbf8
    TestedRecord::new(Scope::Thread, 0x1b, 0x800, 0x800),
    // 00f9cc0e
    TestedRecord::new(Scope::Thread, 0x1b, 0x400, 0x400).platform(Platform::any(0x10)),
    // 00f9cc28
    TestedRecord::new(Scope::Thread, 0xc001_0015, 0x6000, 0x6000),
    // 00f9cc3e
    TestedRecord::new(Scope::Thread, 0xc001_0015, 0x1_0000_0000, 0x1_0000_0000).platform(Platform::any(0x20)),
    // 00f9cc58
    TestedRecord::new(Scope::Thread, 0xc001_0015, 0x800_6000, 0x800_6000).cpu(0x0f00_0000, 0x1313),
    // 00f9ccaf
    TestedRecord::new(Scope::Thread, 0xc001_0400, 0x200_0600, 0x200_0600),
    // 00f9ccc5
    TestedRecord::new(Scope::Thread, 0xc001_0400, 0x200_0600, 0x200_0600).cpu(0x0f00_0000, 0x1313),
    // 00f9ccff
    TestedRecord::new(Scope::Thread, 0xc001_0402, 0x8, 0x0).cpu(0x0f00_0000, 0x1313),
    // 00f9cde7
    TestedRecord::new(Scope::Thread, 0xc001_109b, 0xf00, 0x800).cpu(0x0f00_0000, 0x1313),
    // 00f9ce3e
    TestedRecord::new(Scope::Thread, 0xc001_1002, 0x200, 0x200).cpu(0x0f00_0000, 0x1313),
    // 00f9d01d
    TestedRecord::new(Scope::Thread, 0xc000_2074, 0x187_0000_0200, 0x5_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_2075, 0xffff_0fff_0000_0000, 0x7_00b0_0000_0000),
    // 00f9d04e
    TestedRecord::new(Scope::Thread, 0xc000_2084, 0x187_0000_0200, 0x5_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_2085, 0xffff_0fff_0000_0000, 0x7_00b0_0000_0000),
    // 00f9d07f
    TestedRecord::new(Scope::Thread, 0xc000_2094, 0x187_0000_0200, 0x5_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_2095, 0xffff_0fff_0000_0000, 0x7_00b0_0000_0000),
    // 00f9d0b0
    TestedRecord::new(Scope::Thread, 0xc000_20a4, 0x187_0000_0200, 0x5_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_20a5, 0xffff_0fff_0000_0000, 0x7_00b0_0000_0000),
    // 00f9d0e1
    TestedRecord::new(Scope::Thread, 0xc000_20b4, 0x187_0000_0200, 0x5_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_20b5, 0xffff_0fff_0000_0000, 0x7_00b0_0000_0000),
    // 00f9d112
    TestedRecord::new(Scope::Thread, 0xc000_20c4, 0x187_0000_0200, 0x5_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_20c5, 0xffff_0fff_0000_0000, 0x7_00b0_0000_0000),
    // 00f9d143
    TestedRecord::new(Scope::Thread, 0xc000_20d4, 0x187_0000_0200, 0x5_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_20d5, 0xffff_0fff_0000_0000, 0x7_00b0_0000_0000),
    // 00f9d174
    TestedRecord::new(Scope::Thread, 0xc000_20e4, 0x187_0000_0200, 0x5_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_20e5, 0xffff_0fff_0000_0000, 0x7_00b0_0000_0000),
    // 00f9d63d
    TestedRecord::new(Scope::Thread, 0xc000_21e4, 0x187_0000_0200, 0x5_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_21e5, 0xffff_0fff_0000_0000, 0x1_002e_0000_0000),
    // 00f9d6d0
    TestedRecord::new(Scope::Thread, 0xc000_2174, 0x187_0000_0200, 0x5_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_2175, 0xffff_0fff_0000_0000, 0x2_002e_0000_0000),
    // 00f9d701
    TestedRecord::new(Scope::Thread, 0xc000_2184, 0x187_0000_0200, 0x5_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_2185, 0xffff_0fff_0000_0000, 0x2_002e_0000_0000),
    // 00f9d858
    TestedRecord::new(Scope::Thread, 0xc000_2154, 0x187_0000_0200, 0x5_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_2155, 0xffff_0fff_0000_0000, 0x96_0000_0000),
    // 00f9d889
    TestedRecord::new(Scope::Thread, 0xc000_2164, 0x187_0000_0200, 0x5_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_2165, 0xffff_0fff_0000_0000, 0x96_0000_0000),
    // 00f9d97e
    TestedRecord::new(Scope::Thread, 0xc000_21e4, 0x183_0000_0200, 0x1_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_21e5, 0xffff_0fff_0000_0000, 0xfd_0000_0000),
    // 00f9da42
    TestedRecord::new(Scope::Thread, 0xc000_21e4, 0x183_0000_0200, 0x1_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_21e5, 0xffff_0fff_0000_0000, 0x1_0001_0000_0000),
    // 00f9dc2c
    TestedRecord::new(Scope::Thread, 0xc000_20f4, 0x183_0000_0200, 0x1_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_20f5, 0xffff_0fff_0000_0000, 0x2_0001_0000_0000),
    // 00f9dcbf
    TestedRecord::new(Scope::Thread, 0xc000_21d4, 0x187_0000_0200, 0x5_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_21d5, 0xffff_0fff_0000_0000, 0x18_0000_0000),
    // 00f9dd83
    TestedRecord::new(Scope::Thread, 0xc001_041d, 0x20, 0x20).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_21d5, 0xffff_0fff_0000_0000, 0x18_0000_0000),
    // 00f9de16
    TestedRecord::new(Scope::Thread, 0xc000_21a4, 0x187_0000_0200, 0x5_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_21a5, 0xffff_0fff_0000_0000, 0x1_0046_0000_0000),
    // 00f9de47
    TestedRecord::new(Scope::Thread, 0xc000_21b4, 0x187_0000_0200, 0x5_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_21b5, 0xffff_0fff_0000_0000, 0x1_0046_0000_0000),
    // 00f9de78
    TestedRecord::new(Scope::Thread, 0xc000_21d4, 0x187_0000_0200, 0x5_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_21d5, 0xffff_0fff_0000_0000, 0x6c_0000_0000),
    // 00f9df0b
    TestedRecord::new(Scope::Thread, 0xc000_21d4, 0x187_0000_0200, 0x5_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_21d5, 0xffff_0fff_0000_0000, 0x80_0000_0000),
    // 00f9dfcf
    TestedRecord::new(Scope::Thread, 0xc000_21f4, 0x187_0000_0200, 0x5_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_21f5, 0xffff_0fff_0000_0000, 0x259_0000_0000),
    // 00f9e126
    TestedRecord::new(Scope::Thread, 0xc000_21e4, 0x183_0000_0200, 0x1_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_21e5, 0xffff_0fff_0000_0000, 0xf9_0000_0000),
    // 00f9e1b9
    TestedRecord::new(Scope::Thread, 0xc000_21e4, 0x183_0000_0200, 0x1_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_21e5, 0xffff_0fff_0000_0000, 0x12_0000_0000),
    // 00f9e1ea
    TestedRecord::new(Scope::Thread, 0xc000_21f4, 0x183_0000_0200, 0x1_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_21f5, 0xffff_0fff_0000_0000, 0x2_00ff_0000_0000),
    // 00f9e2ae
    TestedRecord::new(Scope::Thread, 0xc000_2144, 0x183_0000_0200, 0x1_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_2145, 0xffff_0fff_0000_0000, 0xbe_0000_0000),
    // 00f9e372
    TestedRecord::new(Scope::Thread, 0xc000_21a4, 0x183_0000_0200, 0x1_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_21a5, 0xffff_0fff_0000_0000, 0x1e1_0000_0000),
    // 00f9e3a3
    TestedRecord::new(Scope::Thread, 0xc000_21b4, 0x183_0000_0200, 0x1_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_21b5, 0xffff_0fff_0000_0000, 0x1e1_0000_0000),
    // 00f9e3d4
    TestedRecord::new(Scope::Thread, 0xc000_21c4, 0x183_0000_0200, 0x1_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_21c5, 0xffff_0fff_0000_0000, 0x1e1_0000_0000),
    // 00f9e405
    TestedRecord::new(Scope::Thread, 0xc000_20f4, 0x183_0000_0200, 0x1_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_20f5, 0xffff_0fff_0000_0000, 0x5c_0000_0000),
    // 00f9e436
    TestedRecord::new(Scope::Thread, 0xc000_2104, 0x183_0000_0200, 0x1_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_2105, 0xffff_0fff_0000_0000, 0x5c_0000_0000),
    // 00f9e467
    TestedRecord::new(Scope::Thread, 0xc000_2114, 0x183_0000_0200, 0x1_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_2115, 0xffff_0fff_0000_0000, 0x5c_0000_0000),
    // 00f9e498
    TestedRecord::new(Scope::Thread, 0xc000_2124, 0x183_0000_0200, 0x1_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_2125, 0xffff_0fff_0000_0000, 0x5c_0000_0000),
    // 00f9e4c9
    TestedRecord::new(Scope::Thread, 0xc000_2134, 0x183_0000_0200, 0x1_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_2135, 0xffff_0fff_0000_0000, 0x5c_0000_0000),
    // 00f9e4fa
    TestedRecord::new(Scope::Thread, 0xc000_2144, 0x183_0000_0200, 0x1_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_2145, 0xffff_0fff_0000_0000, 0x5c_0000_0000),
    // 00f9e52b
    TestedRecord::new(Scope::Thread, 0xc000_2154, 0x183_0000_0200, 0x1_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_2155, 0xffff_0fff_0000_0000, 0x5c_0000_0000),
    // 00f9e55c
    TestedRecord::new(Scope::Thread, 0xc000_2164, 0x183_0000_0200, 0x1_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_2165, 0xffff_0fff_0000_0000, 0x5c_0000_0000),
    // 00f9e58d
    TestedRecord::new(Scope::Thread, 0xc000_2174, 0x183_0000_0200, 0x1_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_2175, 0xffff_0fff_0000_0000, 0x5c_0000_0000),
    // 00f9e5be
    TestedRecord::new(Scope::Thread, 0xc000_2184, 0x183_0000_0200, 0x1_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_2185, 0xffff_0fff_0000_0000, 0x5c_0000_0000),
    // 00f9e5ef
    TestedRecord::new(Scope::Thread, 0xc000_2194, 0x183_0000_0200, 0x1_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_2195, 0xffff_0fff_0000_0000, 0x5c_0000_0000),
    // 00f9e715
    TestedRecord::new(Scope::Thread, 0xc000_2144, 0x187_0000_0200, 0x5_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_2145, 0xffff_0fff_0000_0000, 0x157_0000_0000),
    // 00f9e7a8
    TestedRecord::new(Scope::Thread, 0xc000_2144, 0x187_0000_0200, 0x5_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_2145, 0xffff_0fff_0000_0000, 0x164_0000_0000),
    // 00f9e86c
    TestedRecord::new(Scope::Thread, 0xc000_21c4, 0x187_0000_0200, 0x5_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_21c5, 0xffff_0fff_0000_0000, 0x50_0000_0000),
    // 00f9e89d
    TestedRecord::new(Scope::Thread, 0xc000_21d4, 0x183_0000_0200, 0x1_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_21d5, 0xffff_0fff_0000_0000, 0xaa_0000_0000),
    // 00f9e8ce
    TestedRecord::new(Scope::Thread, 0xc000_2194, 0x187_0000_0200, 0x5_0000_0000).cpu(0x0f00_0000, 0x1013).msr_test(0xc000_2195, 0xffff_0fff_0000_0000, 0x1e0_0000_0000),
    // 00f9ca60
    TestedRecord::new(Scope::Core, 0xc001_1020, 0x1c_4000_0000_0000, 0x4_4000_0000_0000).cpu(0x0f00_0000, 0x1313),
    // 00f9cad4
    TestedRecord::new(Scope::Core, 0xc001_1023, 0x2_0000_0000_0000, 0x2_0000_0000_0000).cpu(0x0f00_0000, 0x1313),
    // 00f9cb48
    TestedRecord::new(Scope::Core, 0xc001_10e7, 0x40, 0x40).cpu(0x0f00_0000, 0x1313),
    // 00f9cbbc
    TestedRecord::new(Scope::Core, 0xc001_10fb, 0x10_0000, 0x0).cpu(0x0100_0000, 0x3),
    // 00f9cbd9
    TestedRecord::new(Scope::Core, 0xc001_0010, 0x14_0000, 0x14_0000),
    // 00f9e920
    TestedRecord::new(Scope::Complex, 0xc000_2004, 0x187_0000_0200, 0x5_0000_0000).cpu(0x0f00_0000, 0x1313),
    // 00f9e994
    TestedRecord::new(Scope::Complex, 0xc000_2014, 0x183_0000_0200, 0x1_0000_0000).cpu(0x0f00_0000, 0x1313),
    // 00f9ea08
    TestedRecord::new(Scope::Complex, 0xc000_2024, 0x187_0000_0200, 0x5_0000_0000).cpu(0x0f00_0000, 0x1313),
    // 00f9ea7c
    TestedRecord::new(Scope::Complex, 0xc000_2034, 0x183_0000_0200, 0x1_0000_0000).cpu(0x0f00_0000, 0x1313),
    // 00f9eaf0
    TestedRecord::new(Scope::Complex, 0xc000_2054, 0x183_0000_0200, 0x1_0000_0000).cpu(0x0f00_0000, 0x1313),
    // 00f9eb64
    TestedRecord::new(Scope::Complex, 0xc000_2064, 0x183_0000_0200, 0x1_0000_0000).cpu(0x0f00_0000, 0x1313),
];

/// CPUID -> SoC logical ID rows of the WH table families:
/// (ext_model, model_lo, stepping, soc_family, soc_revision).
/// AGESA rule: a family row matches on (family, ext_model) if any of its
/// rows has this model_lo; the result is the exact (model_lo, stepping) row,
/// else the FIRST row of the family (not the first row of the model).
#[rustfmt::skip]
pub static LOGICAL: &[(u8, u8, u8, u32, u16)] = &[
    (0x5, 0x1, 0x0, 0x0100_0000, 0x10),  // default row
    (0x5, 0x8, 0x0, 0x0100_0000, 0x200),
    (0x5, 0x0, 0xb, 0x0100_0000, 0x2),
    (0x5, 0x0, 0x0, 0x0100_0000, 0x1),
    (0xc, 0x0, 0x0, 0x0200_0000, 0x1),  // default row
    (0xc, 0x8, 0x0, 0x0200_0000, 0x100),
    (0x9, 0x0, 0x0, 0x0400_0000, 0x1),  // default row
    (0x9, 0x8, 0x0, 0x0400_0000, 0x1000),
    (0x9, 0xc, 0x0, 0x0400_0000, 0x1),
    (0xa, 0x0, 0x0, 0x0800_0000, 0x1),  // default row
    (0xa, 0xc, 0x0, 0x0800_0000, 0x1),
];
