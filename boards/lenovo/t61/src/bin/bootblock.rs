//! T61 reset through DRAM training and postcar loading.
#![no_std]
#![no_main]
use fstart_board_lenovo_t61::{
    Board,
    mainboard::{self, SmbusBranch},
};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;
use fstart_platform_intel::{BootPath, bootblock::Bootblock};
static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();
fstart_platform_intel::entry!(main, reset_vector);
fn main() -> ! {
    let bb = Bootblock::<Board>::enter();
    let mut pins = mainboard::Pins::of(bb.southbridge().gpio());
    mainboard::setup_dock_console();
    let bb = bb.console(&CONSOLE, mainboard::console());
    if mainboard::select_smbus(&mut pins.smbus_mux, SmbusBranch::SpdAndClock).is_err() {
        bb.fail("SPD mux");
    }
    let mut bb = bb.train_memory().post_dram();
    if bb.boot_path() != BootPath::S3Resume && mainboard::init_ck505(bb.smbus()).is_err() {
        fstart_log::error!("T61: CK505 initialization failed");
    }
    if mainboard::select_smbus(&mut pins.smbus_mux, SmbusBranch::Eeprom).is_err() {
        bb.fail("EEPROM mux");
    }
    bb.load_postcar()
}
