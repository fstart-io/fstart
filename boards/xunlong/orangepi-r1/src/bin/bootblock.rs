//! Orange Pi R1 SRAM bootblock: clocks, console, DRAM, then the DRAM stage.
//! There is no board work: the SoC's flow is the whole `main`.

#![no_std]
#![no_main]

use fstart_board_orangepi_r1::{self as board, ORANGEPI_R1_H3};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;

static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

fstart_platform_sunxi::entry!(main);

fn main() -> ! {
    fstart_platform_sunxi::h3::bootblock(&ORANGEPI_R1_H3, &CONSOLE, board::console())
}
