//! Banana Pi M1 SRAM bootblock: clocks, console, DRAM, then the DRAM stage.
//! There is no board work: the SoC's flow is the whole `main`.

#![no_std]
#![no_main]

use fstart_board_bananapi_m1::{self as board, BANANAPI_M1_A20};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;

static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

fstart_platform_sunxi::entry!(main);

fn main() -> ! {
    fstart_platform_sunxi::a20::bootblock(&BANANAPI_M1_A20, &CONSOLE, board::console())
}
