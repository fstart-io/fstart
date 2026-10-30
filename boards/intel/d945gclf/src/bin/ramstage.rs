//! D945GCLF ramstage: chipset, devices, tables and the payload, or the OS
//! wake vector on S3 resume.

#![no_std]
#![no_main]

use fstart_board_intel_d945gclf::Board;
use fstart_board_intel_d945gclf::mainboard::{self, D945GCLF_SMBIOS_IDENTITY};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;
use fstart_platform_intel::ramstage::{Path, Ramstage};

static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

fstart_platform_intel::entry!(main);

fn main() -> ! {
    let rs = Ramstage::<Board>::enter();
    if mainboard::setup_superio().is_err() {
        rs.fail("SuperIO setup");
    }
    let rs = rs
        .console(&CONSOLE, mainboard::console())
        .memory_map()
        .scan_bus(&[])
        .init_chipset()
        .start_cpus()
        .verify_boot_media()
        .init_display();
    let rs = match rs.boot_path() {
        Path::Cold(rs) => rs.emit_tables(&D945GCLF_SMBIOS_IDENTITY, &mainboard::Acpi),
        Path::Resume(rs) => rs.keep_tables(),
    };
    // Leave the SuperIO's keyboard controller quiet before the OS probes it.
    fstart_driver_superio::quiesce_i8042_for_os();
    rs.boot()
}
