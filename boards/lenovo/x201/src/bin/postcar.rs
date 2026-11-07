//! X201 postcar: CAR is gone; load the ramstage from DRAM.

#![no_std]
#![no_main]

use fstart_board_lenovo_x201::Board;
use fstart_core::services::NoConsole;
use fstart_log::StaticConsole;

// The platform installs the debug consoles selected by `fbuild --debug`.
static CONSOLE: StaticConsole<NoConsole> = StaticConsole::new();

fstart_platform_intel::entry!(main);

fn main() -> ! {
    fstart_platform_intel::postcar::run::<Board, _>(&CONSOLE, ())
}
