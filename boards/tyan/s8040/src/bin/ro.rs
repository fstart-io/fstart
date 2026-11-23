// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Tyan S8040 immutable RO selector stage (PSP BIOS directory entry 0x62).
//!
//! Runs with SPI100 burst prefetch + WP MTRR caching, verifies the active RW
//! slot (Ed25519 + SHA-256), decompresses RW `stage` into
//! `0x75c8_0000..0x75f8_0000`, and jumps to it.

#![no_std]
#![no_main]

use fstart_board_tyan_s8040::{Board, bmc};

fstart_platform_amd::entry!(main);

fn main() -> ! {
    fstart_platform_amd::ab::ro::run_siena_ro::<Board>(bmc::early_init, bmc::CONSOLE)
}
