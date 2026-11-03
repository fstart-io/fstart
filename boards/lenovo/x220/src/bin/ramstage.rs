//! X220 ramstage: chipset, devices, tables and the payload, or the OS wake
//! vector on S3 resume.

#![no_std]
#![no_main]

use fstart_board_lenovo_x220::{Board, mainboard};
use fstart_core::services::NoConsole;
use fstart_log::StaticConsole;
use fstart_platform_intel::ramstage::{Path, Ramstage};

// The platform installs the debug consoles selected by `fbuild --debug`.
static CONSOLE: StaticConsole<NoConsole> = StaticConsole::new();

fstart_platform_intel::entry!(main);

fn main() -> ! {
    let rs = Ramstage::<Board>::enter();
    let mut pins = mainboard::Pins::of(rs.southbridge().gpio());
    let rs = rs
        .console(&CONSOLE, ())
        .memory_map()
        .scan_bus(&mainboard::IO_RESOURCES);
    let mut rs = rs.init_chipset();
    let resume = rs.is_resume();
    let tpm = rs.init_tis_tpm();
    let ec = rs.resolve_set(&mainboard::EC, |_| Ok(()));
    let (identity, acpi) = mainboard::init_devices(&mut pins, rs.smbus(), &ec, tpm.family, resume);
    let rs = rs.start_cpus().verify_boot_media().init_display();
    let rs = match rs.boot_path() {
        Path::Cold(rs) => {
            mainboard::legacy_ec_events();
            rs.emit_tables(&identity.smbios(), &acpi)
        }
        Path::Resume(rs) => rs.keep_tables(),
    };
    rs.boot()
}
