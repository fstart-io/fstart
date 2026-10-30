//! Orange Pi PC2 DRAM stage: the console, the verified boot media, then
//! the payload.

#![no_std]
#![no_main]

use fstart_board_orangepi_pc2::{self as board, ORANGEPI_PC2_H5};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;

static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

fstart_platform_sunxi::entry!(main, handoff);

fn main(handoff: usize) -> ! {
    fstart_platform_sunxi::h3::main_stage(&ORANGEPI_PC2_H5, &CONSOLE, board::console(), handoff)
}
