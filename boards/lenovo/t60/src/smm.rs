//! T6 EC ownership and dock TCO mailbox. ATI/legacy brightness is unsupported.
use fstart_driver_intel::southbridge::{
    gpio_ich::IchGpio,
    smi::{APM_CNT_ACPI_DISABLE, APM_CNT_ACPI_ENABLE, ICH7_GPE0, IchSmi, IchSmmConfig, PendingSmi},
};
use fstart_driver_lenovo::{ec::Ec, t6::dock};
use fstart_platform_intel::smm::SmmContext;
fn smi() -> IchSmi {
    IchSmi::new(fstart_platform_intel::i945::ICH7_PMBASE as u16, ICH7_GPE0)
}
fn ec_channel() -> Ec {
    Ec::new(if smi().acpi_enabled() {
        crate::T6_H8.resources.smm()
    } else {
        crate::T6_H8.resources.os
    })
}
fn dock_command(command: u8) -> Option<u8> {
    let legacy = !IchGpio::new(fstart_driver_intel::ich7::DEFAULT_GPIOBASE as u16)
        .get(dock::LEGACY_MODULE_GPIO as u8);
    dock::smm_dock_command(ec_channel(), command, legacy)
}
fn handle_event() {
    // Once ACPI owns the query channel, SMM must not consume OS events.
    if smi().acpi_enabled() {
        return;
    }
    if let Some(event) = Ec::new(crate::T6_H8.resources.os).query_event() {
        if let Some(command) = dock::smm_event_command(event) {
            let _ = dock_command(command);
        }
    }
}
/// Route the EC's SCI GPI to SMI or SCI as the OS takes or gives up ACPI.
fn acpi_switch(command: u8) {
    let acpi = match command {
        APM_CNT_ACPI_ENABLE => true,
        APM_CNT_ACPI_DISABLE => false,
        _ => return,
    };
    // SAFETY: rendezvous owns the decoded PM and LPC register windows.
    unsafe { smi().route_gpi(crate::EC_SCI_GPIO, acpi) };
}

/// The ICH's SMI sources plus the EC's events and the dock's TCO mailbox.
fn smi_handler(_ctx: &mut SmmContext<'_>, config: &IchSmmConfig) {
    // SAFETY: bound as the SMI handler, so it owns the rendezvous.
    let pending = unsafe { PendingSmi::take(config) };
    if let Some(command) = pending.apm {
        acpi_switch(command);
    }
    if let Some(response) = pending.tco.and_then(dock_command) {
        pending.tco_reply(response);
    }
    if pending.gpi & (1 << crate::EC_SCI_GPIO) != 0
        || pending.gpe & (1 << (crate::EC_SCI_GPIO + 16)) != 0
    {
        handle_event();
    }
    pending.finish();
}

fstart_platform_intel::smm::smm_bin!(handler: smi_handler);
