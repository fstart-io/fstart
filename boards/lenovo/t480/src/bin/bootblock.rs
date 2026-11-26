// SPDX-License-Identifier: MIT OR Apache-2.0
//! ThinkPad T480 bootblock: reset vector, MEC1653 early init, DRAM training,
//! and postcar handoff.
#![no_std]
#![no_main]

use fstart_board_lenovo_t480::{Board, mainboard};
use fstart_core::services::NoConsole;
use fstart_log::StaticConsole;
use fstart_platform_intel::bootblock::Bootblock;

// The platform installs the debug consoles selected by `fbuild --debug`.
static CONSOLE: StaticConsole<NoConsole> = StaticConsole::new();

fstart_platform_intel::entry!(main, reset_vector);

fn main() -> ! {
    let bb = Bootblock::<Board>::enter();
    mainboard::early();
    bb.console(&CONSOLE, ())
        .train_memory()
        .post_dram()
        .load_postcar()
}
