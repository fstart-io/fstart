//! MinnowBoard MAX ramstage: SoC devices, CPUs, tables and the payload.

#![no_std]
#![no_main]

use fstart_board_intel_minnowmax::{Board, mainboard};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;
use fstart_platform_intel::ramstage::{Path, Ramstage};

static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

fstart_platform_intel::entry!(main);

fn main() -> ! {
    let rs = Ramstage::<Board>::enter()
        .console(&CONSOLE, mainboard::console())
        .memory_map()
        .scan_bus(&[])
        .init_chipset()
        .start_cpus()
        .verify_boot_media()
        .init_display();
    let rs = match rs.boot_path() {
        Path::Cold(rs) => rs.emit_tables(&mainboard::MINNOWMAX_SMBIOS_IDENTITY, &mainboard::Acpi),
        Path::Resume(rs) => rs.keep_tables(),
    };
    rs.boot()
}
