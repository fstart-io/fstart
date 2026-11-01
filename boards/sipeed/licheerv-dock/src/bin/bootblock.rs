//! Lichee RV Dock SRAM bootblock: clocks, console, DRAM, then the DRAM stage.
//! There is no board work: the SoC's flow is the whole `main`.

#![no_std]
#![no_main]

use fstart_board_licheerv_dock::{self as board, LICHEERV_DOCK_D1};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;

static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

fstart_platform_sunxi::entry!(main);

fn main() -> ! {
    fstart_platform_sunxi::d1::bootblock(&LICHEERV_DOCK_D1, &CONSOLE, board::console())
}
