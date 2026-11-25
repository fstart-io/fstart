// SPDX-License-Identifier: MIT OR Apache-2.0
//! Peppy bootblock: reset vector, CAR, native DDR3L raminit, postcar handoff.
#![no_std]
#![no_main]

use fstart_board_google_peppy::Board;
use fstart_core::services::NoConsole;
use fstart_log::StaticConsole;
use fstart_platform_intel::bootblock::Bootblock;

static CONSOLE: StaticConsole<NoConsole> = StaticConsole::new();
fstart_platform_intel::entry!(main, reset_vector);

fn main() -> ! {
    let bb = Bootblock::<Board>::enter()
        .console(&CONSOLE, ())
        .train_memory();
    fstart_board_google_peppy::slippy::post(0xda);
    bb.post_dram().load_postcar()
}
