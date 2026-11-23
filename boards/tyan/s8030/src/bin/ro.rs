// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Tyan S8030 immutable RO selector stage (PSP BIOS directory entry 0x62).
//!
//! Runs with early FCH SPI100 speedups + WP MTRR caching, verifies the active
//! RW slot (Ed25519 + SHA-256 + optional discrete TPM 2.0 anti-rollback floor),
//! decompresses RW `stage` into `0x75c8_0000..0x75f8_0000`, and jumps to it.

#![no_std]
#![no_main]

use fstart_board_tyan_s8030::{Board, platform};

fstart_platform_amd::entry!(main);

fn main() -> ! {
    fstart_platform_amd::ab::ro::run_sp3_ro::<Board, _>(platform::early_init, platform::CONSOLE)
}
