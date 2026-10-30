//! Lichee RV Dock DRAM stage: the console, the verified boot media, then
//! the payload.

#![no_std]
#![no_main]

use fstart_board_licheerv_dock::{self as board, LICHEERV_DOCK_D1};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;

static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

fstart_platform_sunxi::entry!(main, handoff);

fn main(handoff: usize) -> ! {
    fstart_platform_sunxi::d1::main_stage(&LICHEERV_DOCK_D1, &CONSOLE, board::console(), handoff)
}
