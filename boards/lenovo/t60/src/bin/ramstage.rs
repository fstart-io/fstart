//! T60 ramstage: chipset, devices, tables and payload.

#![no_std]
#![no_main]

extern crate alloc;

use fstart_board_lenovo_t60::{Board, T6_H8, mainboard};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;
use fstart_platform_intel::ramstage::{Path, Ramstage};

static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

fstart_platform_intel::entry!(main);

fn main() -> ! {
    let rs = Ramstage::<Board>::enter();
    let mut legacy_pin = rs
        .southbridge()
        .gpio()
        .pin(fstart_driver_lenovo::t6::dock::LEGACY_MODULE_GPIO);
    let mut bdc = rs
        .southbridge()
        .gpio()
        .pin(fstart_driver_lenovo::t6::BDC_PRESENCE_GPIO);
    let console_failed = mainboard::setup_console(&mut legacy_pin);
    let rs = rs.console(&CONSOLE, mainboard::console()).memory_map();
    let resources: alloc::vec::Vec<_> =
        fstart_driver_lenovo::t6::firmware_resources(&T6_H8).collect();
    let mut rs = rs.scan_bus(&resources);
    if rs.is_resume() {
        rs.fail("S3 resume");
    }
    let devices = match mainboard::init_ec_and_bay(&mut bdc, &mut legacy_pin, console_failed) {
        Ok(devices) => devices,
        Err(_) => rs.fail("legacy module GPIO"),
    };
    if let Some(v) = devices.ide_primary {
        rs.southbridge_mut().set_ide_primary_enabled(v);
    }
    let mut rs = rs.init_chipset();
    let identity = match mainboard::init_clock_and_identity(rs.smbus()) {
        Ok(identity) => identity,
        Err(_) => rs.fail("CK505 setup"),
    };
    let rs = rs.start_cpus().verify_boot_media().init_display();
    let rs = match rs.boot_path() {
        Path::Cold(rs) => rs.emit_tables(
            &identity.smbios(),
            &mainboard::Acpi {
                native_brightness: devices.native_brightness,
            },
        ),
        Path::Resume(rs) => rs.keep_tables(),
    };
    mainboard::handoff_ec_events();
    fstart_driver_superio::quiesce_i8042_for_os();
    rs.boot()
}
