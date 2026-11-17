//! T61 EC ownership and Mini Dock TCO mailbox.
use fstart_driver_intel::southbridge::smi::{
    APM_CNT_ACPI_DISABLE, APM_CNT_ACPI_ENABLE, ICH8_GPE0, IchSmi, IchSmmConfig, PendingSmi,
};
use fstart_driver_lenovo::{ec::Ec, h8::H8, t6::dock};
use fstart_platform_intel::smm::SmmContext;
const EC_GPIO: u8 = 2;
fn smi() -> IchSmi {
    IchSmi::new(fstart_driver_intel::ich8::ich8::DEFAULT_PMBASE, ICH8_GPE0)
}
fn ec_channel() -> Ec {
    Ec::new(if smi().acpi_enabled() {
        crate::T61_H8.resources.smm()
    } else {
        crate::T61_H8.resources.os
    })
}
fn dock_command(command: u8) -> Option<u8> {
    dock::smm_dock_command(ec_channel(), command, false)
}
fn handle_ec_event() {
    if smi().acpi_enabled() {
        return;
    }
    let command = match ec_channel().query_event() {
        Some(0x37) => dock::SMI_DOCK_CONNECT,
        Some(0x50) => dock::SMI_DOCK_DISCONNECT,
        // 0x18 is Fn+F9 on the T61, not an undock request.
        _ => return,
    };
    let _ = dock_command(command);
}
fn acpi_switch(command: u8) {
    let acpi = match command {
        APM_CNT_ACPI_ENABLE => true,
        APM_CNT_ACPI_DISABLE => false,
        _ => return,
    };
    // SAFETY: the SMM rendezvous owns PCI config and decoded PM registers.
    unsafe { smi().route_gpi(EC_GPIO, acpi) };
    let _ = H8::new(ec_channel()).reset_event_attention();
}
fn smi_handler(_ctx: &mut SmmContext<'_>, config: &IchSmmConfig) {
    // SAFETY: this is the bound SMI handler and owns the rendezvous.
    let pending = unsafe { PendingSmi::take(config) };
    if let Some(command) = pending.apm {
        acpi_switch(command);
    }
    if let Some(response) = pending.tco.and_then(dock_command) {
        pending.tco_reply(response);
    }
    if pending.gpi & (1 << EC_GPIO) != 0 || pending.gpe & (1 << (EC_GPIO + 16)) != 0 {
        handle_ec_event();
    }
    pending.finish();
}
fstart_platform_intel::smm::smm_bin!(handler: smi_handler);
