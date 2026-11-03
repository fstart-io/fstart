//! Lenovo ThinkPad X201 SMM (coreboot `mainboard/lenovo/x201/smihandler.c`):
//! the EC's dock events before OSPM owns them, the EC channel and SCI
//! routing across ACPI ownership changes, and the EC wake on S3.

use fstart_driver_intel::southbridge::smi::{
    APM_CNT_ACPI_DISABLE, APM_CNT_ACPI_ENABLE, ICH8_GPE0, IchSmi, IchSmmConfig, PendingSmi,
};
use fstart_driver_lenovo::ec::Ec;
use fstart_driver_lenovo::h8::H8;
use fstart_driver_lenovo::x201::{self, DockAction};
use fstart_platform_intel::ironlake::{IBEXPEAK_GPIOBASE, IBEXPEAK_PMBASE};
use fstart_platform_intel::smm::SmmContext;

use crate::config::X201_H8;

fn smi() -> IchSmi {
    IchSmi::new(IBEXPEAK_PMBASE, ICH8_GPE0)
}

/// The EC channel SMM may use: 0x1600/0x1604 once the OS owns ACPI, so it
/// never races the OS on 0x62/0x66.
fn ec_channel() -> Ec {
    Ec::new(if smi().acpi_enabled() {
        X201_H8.resources.smm()
    } else {
        X201_H8.resources.os
    })
}

/// coreboot `mainboard_smi_handle_ec_sci()`.
fn handle_ec_event() {
    // Under ACPI the OS owns EC queries; SMM must not steal its events.
    if smi().acpi_enabled() {
        return;
    }
    let ec = Ec::new(X201_H8.resources.os);
    let Some(event) = ec.query_event() else {
        return;
    };
    let mut connect = fstart_driver_intel::southbridge::gpio_ich::IchGpio::new(IBEXPEAK_GPIOBASE)
        .pin(x201::DOCK_CONNECT_GPIO);
    match x201::smm_ec_event(ec, event) {
        Some(DockAction::Disconnect) => {
            x201::dock_disconnect(ec, &mut connect);
        }
        Some(DockAction::Connect) => x201::smm_dock_connected(ec, &mut connect),
        None => {}
    }
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
    unsafe { smi().route_gpi(x201::EC_SCI_GPI, acpi) };
    let ports = if acpi {
        X201_H8.resources.smm()
    } else {
        X201_H8.resources.os
    };
    let _ = H8::new(Ec::new(ports)).reset_event_attention();
}

/// coreboot `mainboard_smi_sleep()`: S3 wakes on the EC when the OS armed
/// an EC wake source.
fn sleep(state: u8) {
    if state == 3 && x201::smm_wake_from_ec(ec_channel()) {
        // SAFETY: SMM rendezvous owns PCI config and decoded PM registers.
        unsafe { smi().route_gpi(x201::EC_WAKE_GPI, true) };
    }
}

fn smi_handler(_ctx: &mut SmmContext<'_>, config: &IchSmmConfig) {
    // SAFETY: bound as the SMI handler, so it owns the rendezvous.
    let pending = unsafe { PendingSmi::take(config) };
    if let Some(command) = pending.apm {
        acpi_switch(command);
    }
    if pending.gpi & (1 << x201::EC_SCI_GPI) != 0 {
        handle_ec_event();
    }
    if let Some(state) = pending.sleep {
        sleep(state);
    }
    pending.finish();
}

fstart_platform_intel::smm::smm_bin!(handler: smi_handler);
