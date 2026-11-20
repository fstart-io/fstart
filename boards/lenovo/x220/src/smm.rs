//! Lenovo ThinkPad X220 SMM (coreboot `mainboard/lenovo/x220/smihandler.c`):
//! the EC's SCI follows ACPI mode, its events are drained while the OS has
//! not taken ACPI, and S3 keeps the EC wake GPI on SCI when the EC has wake
//! events armed. The PCH handler gates the DRAM reset and enters sleep.

use fstart_driver_intel::southbridge::smi::{
    APM_CNT_ACPI_DISABLE, APM_CNT_ACPI_ENABLE, ICH8_GPE0, IchSmi, IchSmmConfig, PendingSmi,
};
use fstart_driver_lenovo::ec::Ec;
use fstart_driver_lenovo::h8::H8;
use fstart_platform_intel::sandybridge::BD82X6X_PMBASE;
use fstart_platform_intel::smm::SmmContext;

use crate::config::{EC_SCI_GPI, EC_WAKE_GPI, X220_H8};

fn smi() -> IchSmi {
    IchSmi::new(BD82X6X_PMBASE, ICH8_GPE0)
}

/// The EC channel SMM may use: 0x1600/0x1604 once the OS owns ACPI, so it
/// never races the OS on 0x62/0x66.
fn ec_channel() -> Ec {
    Ec::new(if smi().acpi_enabled() {
        X220_H8.resources.smm()
    } else {
        X220_H8.resources.os
    })
}

/// coreboot `mainboard_smi_apmc()`: route the EC's SCI GPI to SMI or SCI
/// as the OS takes or gives up ACPI, and reset the EC's event attention on
/// the channel that now owns it.
fn acpi_switch(command: u8) {
    let acpi = match command {
        APM_CNT_ACPI_ENABLE => true,
        APM_CNT_ACPI_DISABLE => false,
        _ => return,
    };
    // SAFETY: SMM rendezvous owns PCI config and decoded PM registers.
    unsafe { smi().route_gpi(EC_SCI_GPI, acpi) };
    let ports = if acpi {
        X220_H8.resources.smm()
    } else {
        X220_H8.resources.os
    };
    let _ = H8::new(Ec::new(ports)).reset_event_attention();
}

/// coreboot `mainboard_smi_handle_ec_sci()`: consume the event before ACPI
/// owns it; the X220 acts on none.
fn handle_ec_event() {
    if smi().acpi_enabled() {
        return;
    }
    let _ = Ec::new(X220_H8.resources.os).query_event();
}

/// coreboot `mainboard_smi_sleep()`: S3 wakes on the EC when the OS armed
/// an EC wake source.
fn sleep(state: u8) {
    if state == 3 && H8::new(ec_channel()).wake_armed() {
        // SAFETY: SMM rendezvous owns PCI config and decoded PM registers.
        unsafe { smi().route_gpi(EC_WAKE_GPI, true) };
    }
}

fn smi_handler(_ctx: &mut SmmContext<'_>, config: &IchSmmConfig) {
    // SAFETY: bound as the SMI handler, so it owns the rendezvous.
    let pending = unsafe { PendingSmi::take(config) };
    if let Some(command) = pending.apm {
        acpi_switch(command);
    }
    if pending.gpi & (1 << EC_SCI_GPI) != 0 {
        handle_ec_event();
    }
    if let Some(state) = pending.sleep {
        sleep(state);
    }
    pending.finish();
}

fstart_platform_intel::smm::smm_bin!(handler: smi_handler);
