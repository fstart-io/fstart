//! X200 bootblock: from the reset vector in cache-as-RAM to postcar in DRAM.

#![no_std]
#![no_main]

use fstart_board_lenovo_x200::Board;
use fstart_board_lenovo_x200::mainboard::{self, SmbusBranch};
use fstart_core::services::NoConsole;
use fstart_log::StaticConsole;
use fstart_platform_intel::bootblock::Bootblock;

/// The X200 has no serial port: `fbuild --debug` adds the EHCI debug or
/// SPI flash console.
static CONSOLE: StaticConsole<NoConsole> = StaticConsole::new();

fstart_platform_intel::entry!(main, reset_vector);

fn main() -> ! {
    let bb = Bootblock::<Board>::enter();
    let mut pins = mainboard::Pins::of(bb.southbridge().gpio());
    let bb = bb.console(&CONSOLE, ());
    // The GPIO table leaves the SMBus mux on the SPDs for training.
    if mainboard::select_smbus(&mut pins.smbus_mux, SmbusBranch::Spd).is_err() {
        bb.fail("SMBus routing to the SPDs");
    }
    let bb = bb.train_memory().post_dram();
    // coreboot mb_post_raminit_setup(): the mux goes to the EEPROM.
    if mainboard::select_smbus(&mut pins.smbus_mux, SmbusBranch::Eeprom).is_err() {
        bb.fail("SMBus routing to the EEPROM");
    }
    bb.load_postcar()
}
