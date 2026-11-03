//! X220 bootblock: from the reset vector in cache-as-RAM to postcar in DRAM.

#![no_std]
#![no_main]

use fstart_board_lenovo_x220::{Board, mainboard};
use fstart_core::services::NoConsole;
use fstart_log::StaticConsole;
use fstart_platform_intel::bootblock::Bootblock;

// The platform installs the debug consoles selected by `fbuild --debug`.
static CONSOLE: StaticConsole<NoConsole> = StaticConsole::new();

fstart_platform_intel::entry!(main, reset_vector);

fn main() -> ! {
    let mut bb = Bootblock::<Board>::enter().console(&CONSOLE, ());
    // coreboot mainboard_early_init(): lock the EEPROM's RFID side.
    mainboard::early_init(bb.smbus());
    bb.train_memory().post_dram().load_postcar()
}
