//! D945GCLF bootblock: from the reset vector in cache-as-RAM to postcar.

#![no_std]
#![no_main]

use fstart_board_intel_d945gclf::{Board, mainboard};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;
use fstart_platform_intel::bootblock::Bootblock;

static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

fstart_platform_intel::entry!(main, reset_vector);

fn main() -> ! {
    let bb = Bootblock::<Board>::enter();
    if mainboard::setup_superio().is_err() {
        bb.fail("SuperIO setup");
    }
    bb.console(&CONSOLE, mainboard::console())
        .train_memory()
        .post_dram()
        .load_postcar()
}
