//! X61 bootblock: from the reset vector in cache-as-RAM to postcar in DRAM.

#![no_std]
#![no_main]

use fstart_board_lenovo_x61::Board;
use fstart_board_lenovo_x61::mainboard::{self, SmbusBranch};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;
use fstart_platform_intel::BootPath;
use fstart_platform_intel::bootblock::Bootblock;

static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

fstart_platform_intel::entry!(main);

fn main() -> ! {
    let bb = Bootblock::<Board>::enter();
    let mut pins = mainboard::Pins::of(bb.southbridge().gpio());
    // The console UART sits in the UltraBase dock.
    mainboard::setup_dock_console(&mut pins.dock_present);
    let bb = bb.console(&CONSOLE, mainboard::console());

    // The SPDs and the CK505 share one branch of the SMBus mux. This is the
    // only stage that needs it: training reads the SPDs completely and the
    // ramstage gets what it needs from them through the store.
    if mainboard::select_smbus(&mut pins.smbus_mux, SmbusBranch::SpdAndClock).is_err() {
        bb.fail("SMBus routing to the SPDs");
    }
    let mut bb = bb.train_memory().post_dram();
    if bb.boot_path() != BootPath::S3Resume && mainboard::init_ck505(bb.smbus()).is_err()
    {
        fstart_log::error!("lenovo-x61: CK505 programming failed");
    }
    if mainboard::select_smbus(&mut pins.smbus_mux, SmbusBranch::Eeprom).is_err() {
        bb.fail("SMBus routing to the EEPROM");
    }
    bb.load_postcar()
}
