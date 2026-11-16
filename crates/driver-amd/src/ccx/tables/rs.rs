// SPDX-License-Identifier: MIT OR Apache-2.0
//! RS (Genoa, Siena; Zen 4 and Zen 4c) MSR table, generated: do not edit.
//!
//! The reset table of AGESA's CcxProgramTablesAtReset, by
//! `tools.gen_rs_tables` of the sp5 analysis workspace from the TYAN
//! S8040GM2NE-NFX V0.02 AmdCcxZen4Pei (TE sha256 76c54116680e0d92…):
//! tables `ffa2c1a8`, `ffa2c1a0`, `ffa2c27c`, one per scope, in the order
//! the vendor engine (`ffa27b03`) runs them. Each record gives its address
//! in the vendor table. The CPU guards are the logical families of
//! [`crate::ccx::rs::logical`] (0x1000 Genoa, 0x8000 Siena; the others are
//! SoCs fstart does not support); the platform guards are not decoded and
//! [`crate::ccx::rs::thread_init`] passes no flags.

use crate::ccx::table::{Platform, Record};
use crate::ccx::topology::Scope;

/// Phase-1 (cold boot) records in dispatch order.
#[rustfmt::skip]
pub static RECORDS: &[Record] = &[
    // ffa2c3ec
    Record::new(Scope::Thread, 0x1b, 0x800, 0x800),
    // ffa2c402
    Record::new(Scope::Thread, 0x1b, 0x400, 0x400).platform(Platform::any(0x2)),
    // ffa2c41c
    Record::new(Scope::Thread, 0xc001_0015, 0x8006000, 0x8006000),
    // ffa2c432
    Record::new(Scope::Thread, 0xc001_0015, 0x100000000, 0x100000000).platform(Platform::any(0x4)),
    // ffa2c44c
    Record::new(Scope::Thread, 0xc001_0400, 0x380000, 0x380000),
    // ffa2c462
    Record::new(Scope::Thread, 0xc001_0401, 0x10800, 0x10800).cpu(0x2000, 0x11),
    // ffa2c47e
    Record::new(Scope::Thread, 0xc001_0401, 0x10800, 0x10800).cpu(0x4000, 0x11),
    // ffa2c49a
    Record::new(Scope::Thread, 0xc001_0401, 0x10800, 0x10800).cpu(0x1000, 0x77),
    // ffa2c4b6
    Record::new(Scope::Thread, 0xc001_0401, 0x10800, 0x10800).cpu(0x8000, 0x3),
    // ffa2c4d2
    Record::new(Scope::Thread, 0xc001_0402, 0x8, 0x8),
    // ffa2c4e8
    Record::new(Scope::Thread, 0xc001_0402, 0x8, 0x0).cpu(0x1000, 0x77),
    // ffa2c504
    Record::new(Scope::Thread, 0xc001_0402, 0x8, 0x0).cpu(0x8000, 0x3),
    // ffa2c520
    Record::new(Scope::Thread, 0xc001_1000, 0xff000000, 0x33000000).cpu(0x1000, 0x77),
    // ffa2c53c
    Record::new(Scope::Thread, 0xc001_1000, 0xff000000, 0x33000000).cpu(0x8000, 0x3),
    // ffa2c558
    Record::new(Scope::Thread, 0xc001_1002, 0x10, 0x0),
    // ffa2c56e
    Record::new(Scope::Thread, 0xc001_1002, 0x800, 0x0).cpu(0x1000, 0x77),
    // ffa2c58a
    Record::new(Scope::Thread, 0xc001_1002, 0x800, 0x0).cpu(0x2000, 0x11),
    // ffa2c5a6
    Record::new(Scope::Thread, 0xc001_1004, 0x20000000000000, 0x0).platform(Platform::any(0x4)),
    // ffa2c5c0
    Record::new(Scope::Thread, 0xc001_1004, 0x4000000000000000, 0x0).platform(Platform::any(0x8)),
    // ffa2c5da
    Record::new(Scope::Thread, 0xc001_1002, 0x40000, 0x0).platform(Platform::any(0x8)),
    // ffa2c5f4
    Record::new(Scope::Thread, 0xc001_10dc, 0x400000, 0x0),
    // ffa2c60a
    Record::new(Scope::Thread, 0xc001_10dc, 0x80000000, 0x80000000).cpu(0x1000, 0x77).platform(Platform::any(0x10)),
    // ffa2c62a
    Record::new(Scope::Thread, 0xc001_10de, 0x10, 0x0).cpu(0x1000, 0x77),
    // ffa2c646
    Record::new(Scope::Thread, 0xc000_2004, 0x500000000, 0x500000000).cpu(0x2000, 0x11),
    // ffa2c662
    Record::new(Scope::Thread, 0xc000_2014, 0x100000000, 0x100000000).cpu(0x2000, 0x11),
    // ffa2c67e
    Record::new(Scope::Thread, 0xc000_2024, 0x700000000, 0x500000000).cpu(0x2000, 0x11),
    // ffa2c69a
    Record::new(Scope::Thread, 0xc000_2034, 0x100000000, 0x100000000).cpu(0x2000, 0x11),
    // ffa2c6b6
    Record::new(Scope::Thread, 0xc000_2054, 0x100000000, 0x100000000).cpu(0x2000, 0x11),
    // ffa2c6d2
    Record::new(Scope::Thread, 0xc000_2064, 0x100000000, 0x100000000).cpu(0x2000, 0x11),
    // ffa2c6ee
    Record::new(Scope::Thread, 0xc001_1092, 0x200, 0x200).cpu(0x2000, 0x11),
    // ffa2c70a
    Record::new(Scope::Thread, 0xc001_0140, 0xffffffffffffffff, 0x5),
    // ffa2c720
    Record::new(Scope::Thread, 0xc001_0141, 0xffffffffffffffff, 0x0),
    // ffa2c284
    Record::new(Scope::Core, 0xc000_0410, 0x8, 0x8).cpu(0x4000, 0x1),
    // ffa2c2a0
    Record::new(Scope::Core, 0xc001_1020, 0x5c400000000000, 0x4400000000000),
    // ffa2c2b6
    Record::new(Scope::Core, 0xc001_1022, 0x8680000000000000, 0x8680000000000000),
    // ffa2c2cc
    Record::new(Scope::Core, 0xc001_1028, 0x10000000000000, 0x10000000000000).core(0x3, 0x300),
    // ffa2c2e8
    Record::new(Scope::Core, 0xc001_1028, 0x80000000000, 0x80000000000).core(0x3, 0x1100),
    // ffa2c304
    Record::new(Scope::Core, 0xc001_1028, 0x10000000000000, 0x0).cpu(0x8000, 0x3),
    // ffa2c320
    Record::new(Scope::Core, 0xc001_102a, 0x10000000018080, 0x10000000018080),
    // ffa2c336
    Record::new(Scope::Core, 0xc001_102a, 0x10000, 0x0).cpu(0x1000, 0x77),
    // ffa2c352
    Record::new(Scope::Core, 0xc001_102a, 0x10000, 0x0).cpu(0x8000, 0x3),
    // ffa2c36e
    Record::new(Scope::Core, 0xc001_102e, 0x40000000, 0x40000000).cpu(0x4000, 0x1),
    // ffa2c38a
    Record::new(Scope::Core, 0xc001_10e2, 0x2700000000018, 0x2300000000018),
    // ffa2c3a0
    Record::new(Scope::Core, 0xc001_10e5, 0x1100000004000000, 0x1100000004000000),
    // ffa2c3b6
    Record::new(Scope::Core, 0xc001_10e7, 0x88, 0x80),
    // ffa2c3cc
    Record::new(Scope::Core, 0xc001_10e8, 0xfe0, 0xfe0).cpu(0x1000, 0x77),
    // ffa2c1b4
    Record::new(Scope::Complex, 0xc001_1093, 0xdc1f80, 0x501680).cpu(0x1000, 0x77),
    // ffa2c1d0
    Record::new(Scope::Complex, 0xc001_1093, 0xdc1f80, 0x501680).cpu(0x2000, 0x11),
    // ffa2c1ec
    Record::new(Scope::Complex, 0xc001_1097, 0x11c6, 0x1106).cpu(0x1000, 0x77),
    // ffa2c208
    Record::new(Scope::Complex, 0xc001_1097, 0x1e0000, 0x1e0000).cpu(0x1000, 0x77),
    // ffa2c224
    Record::new(Scope::Complex, 0xc001_1097, 0x1e0000, 0x1e0000).cpu(0x4000, 0x11),
    // ffa2c240
    Record::new(Scope::Complex, 0xc001_1097, 0x1e0000, 0x1e0000).cpu(0x10000, 0x1),
    // ffa2c25c
    Record::new(Scope::Complex, 0xc001_100a, 0x100000, 0x100000).cpu(0x4000, 0x11),
];
