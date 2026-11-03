//! X200 postcar: CAR is gone; load the ramstage from DRAM.

#![no_std]
#![no_main]

use fstart_board_lenovo_x200::Board;
use fstart_core::services::NoConsole;
use fstart_log::StaticConsole;

/// The X200 has no serial port: `fbuild --debug` adds the EHCI debug or
/// SPI flash console.
static CONSOLE: StaticConsole<NoConsole> = StaticConsole::new();

fstart_platform_intel::entry!(main);

fn main() -> ! {
    fstart_platform_intel::postcar::run::<Board, _>(&CONSOLE, ())
}
