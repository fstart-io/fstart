//! GA-D510UD ramstage: devices, tables and payload or S3 resume.

#![no_std]
#![no_main]

use fstart_board_gigabyte_ga_d510ud::Board;
use fstart_board_gigabyte_ga_d510ud::mainboard::{self, GA_D510UD_SMBIOS_IDENTITY};
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
        Path::Cold(rs) => rs.emit_tables(&GA_D510UD_SMBIOS_IDENTITY, &mainboard::Acpi),
        Path::Resume(rs) => rs.keep_tables(),
    };
    fstart_driver_superio::quiesce_i8042_for_os();
    rs.boot()
}
