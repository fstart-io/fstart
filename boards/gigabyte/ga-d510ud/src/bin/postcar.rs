//! GA-D510UD postcar: CAR is gone; load the ramstage from DRAM.

#![no_std]
#![no_main]

use fstart_board_gigabyte_ga_d510ud::{Board, mainboard};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;

static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

fstart_platform_intel::entry!(main);

fn main() -> ! {
    fstart_platform_intel::postcar::run::<Board, _>(&CONSOLE, mainboard::console())
}
