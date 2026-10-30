//! X61 ramstage: chipset, devices, tables and the payload, or the OS wake
//! vector on S3 resume.

#![no_std]
#![no_main]

use fstart_board_lenovo_x61::{Board, mainboard};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;
use fstart_platform_intel::ramstage::{Path, Ramstage};

static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

fstart_platform_intel::entry!(main);

fn main() -> ! {
    let rs = Ramstage::<Board>::enter();
    let mut pins = mainboard::Pins::of(rs.southbridge().gpio());
    mainboard::setup_dock_console(&mut pins.dock_present);
    let mut rs = rs
        .console(&CONSOLE, mainboard::console())
        .memory_map()
        .scan_bus(&[]);
    // ICH8 reads the IDE population when it programs the timings.
    let ultrabay = mainboard::ultrabay_populated(&mut pins.dock_present);
    rs.southbridge_mut().set_ide_primary_enabled(ultrabay);
    let mut rs = rs.init_chipset();
    let resume = rs.is_resume();
    let identity = mainboard::init_devices(&mut pins.bluetooth_card, rs.southbridge_mut(), resume);
    let rs = rs.start_cpus().verify_boot_media().init_display();
    let rs = match rs.boot_path() {
        Path::Cold(rs) => rs.emit_tables(&identity.smbios(), &mainboard::Acpi),
        Path::Resume(rs) => rs.keep_tables(),
    };
    // SMM is installed: hand the EC's events to SMM or the OS.
    mainboard::handoff_ec_events(resume);
    // Leave the EC's keyboard controller quiet before the OS probes it.
    fstart_driver_superio::quiesce_i8042_for_os();
    rs.boot()
}
