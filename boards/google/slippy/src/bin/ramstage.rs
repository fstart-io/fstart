// SPDX-License-Identifier: MIT OR Apache-2.0
//! Peppy ramstage: PCI allocation, chipset/TPM/EC init, SMM handoff, display, ACPI/SMBIOS.
#![no_std]
#![no_main]

use fstart_board_google_peppy::{
    Board,
    acpi::{Acpi, IDENTITY},
    options::DEVICES,
    slippy,
};
use fstart_core::services::NoConsole;
use fstart_log::StaticConsole;
use fstart_platform_intel::{
    IntelSouthbridgeDriver,
    ramstage::{Path, Ramstage},
};

static CONSOLE: StaticConsole<NoConsole> = StaticConsole::new();
fstart_platform_intel::entry!(main);

fn main() -> ! {
    let mut rs = Ramstage::<Board>::enter().console(&CONSOLE, ());
    let devices = rs.resolve_set(&DEVICES, |_| Ok(()));
    if !devices.lte_ngff {
        rs.southbridge_mut().disable_root_ports(1 << 1);
    }
    let mut rs = rs.memory_map().scan_bus(&[]).init_chipset();
    let resume = rs.is_resume();
    let tpm = rs.init_tis_tpm();
    let has_smm = fstart_platform_intel::has_smm_image();
    // Transfer EC event ownership before enabling SMIs; the BSP and SMM
    // must not issue overlapping ACPI queries or host commands.
    let routing = slippy::init_ec(resume).filter(|_| has_smm);
    if routing.is_none() {
        rs.southbridge_mut().set_s3_enabled(false);
    }
    let serial_io = rs.southbridge().serial_io_populated();
    let rs = rs.start_cpus();
    if has_smm {
        slippy::handoff_ec(routing);
    }
    let rs = rs.verify_boot_media().init_display();
    match rs.boot_path() {
        Path::Cold(rs) => {
            let onboard = fstart_board_google_peppy::acpi::smbios_onboard(devices.ambient_light);
            rs.emit_tables_with_onboard(
                &IDENTITY,
                &Acpi {
                    devices,
                    tpm: tpm.family,
                    tpm_irq: tpm.irq,
                    serial_io,
                    wlan: true,
                    ec_routed: routing.is_some(),
                },
                &onboard,
            )
        }
        Path::Resume(rs) => rs.keep_tables(),
    }
    .boot()
}
