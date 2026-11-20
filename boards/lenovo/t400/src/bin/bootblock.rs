//! T400-series bootblock: dock serial setup, hybrid/discrete graphics policy,
//! DDR3 training in cache-as-RAM, and postcar loading.

#![no_std]
#![no_main]

use fstart_board_lenovo_t400::Board;
use fstart_board_lenovo_t400::mainboard::{self, SmbusBranch};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;
use fstart_platform_intel::bootblock::Bootblock;

/// Dock PC87384 COM1 serial console; `fbuild --debug` can also add EHCI debug
/// or SPI flash console.
static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

fstart_platform_intel::entry!(main, reset_vector);

fn main() -> ! {
    let mut bb = Bootblock::<Board>::enter();
    let mut pins = mainboard::Pins::of(bb.southbridge().gpio());
    let dock = mainboard::setup_dock_console();
    // `console()` runs GM45 early init, which programs DEVEN. Resolve the
    // board's IGD/PEG policy first so those bits already agree at raminit.
    let mut hg = mainboard::HYBRID_GRAPHICS.board;
    if !fstart_board_lenovo_t400::config::IS_R500 {
        let source = fstart_platform_intel::options::store();
        fstart_log::option::resolve(mainboard::HYBRID_GRAPHICS.knobs, &source, &mut hg, |_| {
            Ok(())
        });
    }
    let graphics =
        mainboard::select_early_graphics(&mut pins.dgpu_detect, &mut pins.dgpu_power, hg.mode);
    if let Ok(policy) = graphics {
        bb.northbridge_mut().set_graphics_policy(policy);
    }
    let bb = bb.console(&CONSOLE, mainboard::console());
    if graphics.is_err() {
        bb.fail("GPU strap GPIO read");
    }
    if dock.is_err() {
        fstart_log::error!("lenovo-t400: dock initialization failed; serial console may be absent");
    }
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
