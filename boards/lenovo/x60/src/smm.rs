//! Lenovo ThinkPad X60 mainboard SMM policy.

use fstart_driver_intel::southbridge::smi::{
    APM_CNT_ACPI_DISABLE, APM_CNT_ACPI_ENABLE, ICH7_GPE0, IchBoardSmmHandler, IchSmi, IchSmmHandler,
};
use fstart_driver_lenovo::ec::Ec;
use fstart_driver_lenovo::h8::H8;
use fstart_driver_lenovo::x6::dock as x6_dock;
use fstart_platform_intel::smm::SmmContext;

use crate::Board;

const EC_GPIO: u8 = crate::config::EC_SCI_GPIO;

/// No mutable port globals: chipset ACPI ownership determines the SMM channel.
fn smi() -> IchSmi {
    IchSmi::new(fstart_platform_intel::i945::ICH7_PMBASE as u16, ICH7_GPE0)
}

fn ec_channel() -> Ec {
    if smi().acpi_enabled() {
        Ec::new(crate::config::X60_H8.resources.smm())
    } else {
        Ec::new(crate::config::X60_H8.resources.os)
    }
}

fn dock_command(command: u8) -> Option<u8> {
    x6_dock::smm_dock_command(ec_channel(), command)
}

fn handle_ec_event() {
    // Under ACPI the OS owns EC queries; SMM must not steal its events.
    if smi().acpi_enabled() {
        return;
    }
    if let Some(event) = Ec::new(crate::config::X60_H8.resources.os).query_event() {
        if let Some(command) = x6_dock::smm_event_command(event) {
            let _ = dock_command(command);
        }
    }
}

pub struct LenovoX60SmmHandler;

impl IchBoardSmmHandler for LenovoX60SmmHandler {
    unsafe fn on_apmc(_ctx: &mut SmmContext<'_>, command: u8) {
        let acpi = match command {
            APM_CNT_ACPI_ENABLE => true,
            APM_CNT_ACPI_DISABLE => false,
            _ => return,
        };
        // SAFETY: SMM rendezvous owns PCI config and decoded PM registers.
        unsafe { smi().route_gpi(EC_GPIO, acpi) };
        // Discard pending events and enable attention on the SMM-owned channel.
        let ports = if acpi {
            crate::config::X60_H8.resources.smm()
        } else {
            crate::config::X60_H8.resources.os
        };
        let _ = H8::new(Ec::new(ports)).reset_event_attention();
    }

    unsafe fn on_gpi(_ctx: &mut SmmContext<'_>, status: u16) {
        if status & (1 << EC_GPIO) != 0 {
            handle_ec_event();
        }
    }

    unsafe fn on_gpe(_ctx: &mut SmmContext<'_>, status: u64) {
        if status & (1 << (EC_GPIO + 16)) != 0 {
            handle_ec_event();
        }
    }

    unsafe fn on_tco_command(_ctx: &mut SmmContext<'_>, command: u8) -> Option<u8> {
        dock_command(command)
    }
}

fstart_platform_intel::smm::smm_bin!(Board, IchSmmHandler<LenovoX60SmmHandler>);
