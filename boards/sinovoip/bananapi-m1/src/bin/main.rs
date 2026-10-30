//! Banana Pi M1 DRAM stage: the console, the verified boot media, then
//! the payload.

#![no_std]
#![no_main]

use fstart_board_bananapi_m1::{self as board, BANANAPI_M1_A20};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;

static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

fstart_platform_sunxi::entry!(main, handoff);

fn main(handoff: usize) -> ! {
    fstart_platform_sunxi::a20::main_stage(&BANANAPI_M1_A20, &CONSOLE, board::console(), handoff)
}
