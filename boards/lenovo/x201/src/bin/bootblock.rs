//! X201 bootblock: from the reset vector in cache-as-RAM to postcar in DRAM.

#![no_std]
#![no_main]

use fstart_board_lenovo_x201::{Board, mainboard};
use fstart_core::services::NoConsole;
use fstart_log::StaticConsole;
use fstart_platform_intel::bootblock::Bootblock;

// The platform installs the debug consoles selected by `fbuild --debug`.
static CONSOLE: StaticConsole<NoConsole> = StaticConsole::new();

fstart_platform_intel::entry!(main, reset_vector);

fn main() -> ! {
    let bb = Bootblock::<Board>::enter();
    // Power USB before the optional EHCI debug console starts. An absent
    // gadget must not stop boot before the SPI flash logger can be installed.
    mainboard::early_usb_power();
    let mut bb = bb.console(&CONSOLE, ());
    // coreboot mainboard_pre_raminit(): the clock generator before the
    // SPDs are read.
    if mainboard::set_fsb_frequency(bb.smbus()).is_err() {
        bb.fail("clock generator FSB programming");
    }
    bb.train_memory().post_dram().load_postcar()
}
