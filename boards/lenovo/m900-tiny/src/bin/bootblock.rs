// SPDX-License-Identifier: MIT OR Apache-2.0
//! M900 Tiny reset through DRAM training and postcar loading.
#![no_std]
#![no_main]
use fstart_board_lenovo_m900_tiny::{Board, mainboard};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;
use fstart_platform_intel::bootblock::Bootblock;

static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

fstart_platform_intel::entry!(main, reset_vector);

fn main() -> ! {
    let bb = Bootblock::<Board>::enter();
    if mainboard::early().is_err() {
        bb.fail("Super I/O");
    }
    bb.console(&CONSOLE, mainboard::console())
        .train_memory()
        .post_dram()
        .load_postcar()
}
