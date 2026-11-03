//! X201 bootblock: from the reset vector in cache-as-RAM to postcar in DRAM.

#![no_std]
#![no_main]

use fstart_board_lenovo_x201::{Board, mainboard};
use fstart_driver_usbdebug::EhciDebug;
use fstart_log::StaticConsole;
use fstart_platform_intel::bootblock::Bootblock;

static CONSOLE: StaticConsole<EhciDebug> = StaticConsole::new();

fstart_platform_intel::entry!(main, reset_vector);

fn main() -> ! {
    let bb = Bootblock::<Board>::enter();
    // The X201 has no serial port: the console is the EHCI debug port,
    // which needs the EC's USB power first.
    mainboard::early_usb_power();
    let mut bb = bb.console(&CONSOLE, mainboard::console());
    // coreboot mainboard_pre_raminit(): the clock generator before the
    // SPDs are read.
    if mainboard::set_fsb_frequency(bb.smbus()).is_err() {
        bb.fail("clock generator FSB programming");
    }
    bb.train_memory().post_dram().load_postcar()
}
