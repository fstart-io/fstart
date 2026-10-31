//! D41S bootblock: from cache-as-RAM to postcar.

#![no_std]
#![no_main]

use fstart_board_foxconn_d41s::{Board, mainboard};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;
use fstart_platform_intel::{BootPath, bootblock::Bootblock};

static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

fstart_platform_intel::entry!(main);

fn main() -> ! {
    let bb = Bootblock::<Board>::enter();
    if mainboard::setup_console().is_err() {
        bb.fail("setup_console");
    }
    let mut bb = bb
        .console(&CONSOLE, mainboard::console())
        .train_memory()
        .post_dram();
    if bb.boot_path() != BootPath::S3Resume {
        mainboard::setup_clock(bb.smbus());
    }
    bb.load_postcar()
}
