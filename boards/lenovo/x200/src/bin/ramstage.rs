//! X200 ramstage: chipset, devices, tables and the payload, or the OS wake
//! vector on S3 resume.

#![no_std]
#![no_main]

use fstart_board_lenovo_x200::{Board, mainboard};
use fstart_core::services::NoConsole;
use fstart_log::StaticConsole;
use fstart_platform_intel::ramstage::{Path, Ramstage};

/// The X200 has no serial port: `fbuild --debug` adds the EHCI debug or
/// SPI flash console.
static CONSOLE: StaticConsole<NoConsole> = StaticConsole::new();

fstart_platform_intel::entry!(main);

fn main() -> ! {
    let rs = Ramstage::<Board>::enter();
    let mut pins = mainboard::Pins::of(rs.southbridge().gpio());
    let rs = rs.console(&CONSOLE, ()).memory_map().scan_bus(&[]);
    let mut rs = rs.init_chipset();
    let resume = rs.is_resume();
    let (identity, acpi) = mainboard::init_devices(&mut pins, rs.smbus(), resume);
    let rs = rs.start_cpus().verify_boot_media().init_display();
    let rs = match rs.boot_path() {
        Path::Cold(rs) => rs.emit_tables(&identity.smbios(), &acpi),
        Path::Resume(rs) => rs.keep_tables(),
    };
    rs.boot()
}
