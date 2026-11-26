// SPDX-License-Identifier: MIT OR Apache-2.0
//! ThinkPad T480 ramstage: chipset, EC/PMH7/TPM, CPUs, display, tables, and
//! payload handoff (or S3 wake vector on resume).
#![no_std]
#![no_main]

use fstart_board_lenovo_t480::{Board, mainboard};
use fstart_core::services::NoConsole;
use fstart_log::StaticConsole;
use fstart_platform_intel::ramstage::{Path, Ramstage};

// The platform installs the debug consoles selected by `fbuild --debug`.
static CONSOLE: StaticConsole<NoConsole> = StaticConsole::new();

fstart_platform_intel::entry!(main);

fn main() -> ! {
    let mut rs = Ramstage::<Board>::enter()
        .console(&CONSOLE, ())
        .memory_map()
        .scan_bus(&[])
        .init_chipset();
    let resume = rs.is_resume();
    let tpm = rs.init_tis_tpm();
    let ec = rs.resolve_set(&mainboard::EC, |_| Ok(()));
    let (identity, acpi) = mainboard::init_devices(rs.southbridge(), &ec, tpm.family, resume);
    let rs = rs.start_cpus().verify_boot_media().init_display();
    let rs = match rs.boot_path() {
        Path::Cold(rs) => rs.emit_tables(&identity.smbios(), &acpi),
        Path::Resume(rs) => rs.keep_tables(),
    };
    rs.boot()
}
