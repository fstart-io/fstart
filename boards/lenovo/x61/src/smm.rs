//! Lenovo ThinkPad X61 mainboard SMM policy.

use fstart_driver_intel::southbridge::smi::{
    APM_CNT_ACPI_DISABLE, APM_CNT_ACPI_ENABLE, ICH8_GPE0, IchBoardSmmHandler, IchSmi, IchSmmHandler,
};
use fstart_driver_lenovo::ec::Ec;
use fstart_driver_lenovo::h8::{H8, H8Led, H8LedMode};
use fstart_platform_intel::smm::SmmContext;

use crate::{Board, mainboard::dock};

pub const SMI_DOCK_CONNECT: u8 = 0x01;
pub const SMI_DOCK_DISCONNECT: u8 = 0x02;
const EC_GPIO: u8 = 2;

/// No mutable port globals: chipset ACPI ownership determines the SMM channel.
fn smi() -> IchSmi {
    IchSmi::new(fstart_driver_intel::ich8::ich8::DEFAULT_PMBASE, ICH8_GPE0)
}

fn ec_channel() -> Ec {
    if smi().acpi_enabled() {
        Ec::new(crate::config::X61_H8.resources.smm())
    } else {
        Ec::new(crate::config::X61_H8.resources.os)
    }
}

fn dock_command(command: u8) -> Option<u8> {
    let ec = ec_channel();
    let h8 = H8::new(ec);
    match command {
        SMI_DOCK_CONNECT => {
            let handshake = h8.dock_latch(false);
            fstart_arch::udelay(250_000);
            let connected = handshake && dock::dock_connect_with_ec(ec).is_ok();
            if connected {
                let state = h8.dock_latch(true);
                let led_off = h8.set_led(H8Led::Dock2, H8LedMode::Off);
                let led_on = h8.set_led(H8Led::Dock1, H8LedMode::On);
                Some(u8::from(state && led_off && led_on))
            } else {
                let _ = h8.set_led(H8Led::Dock1, H8LedMode::Off);
                let _ = h8.set_led(H8Led::Dock2, H8LedMode::Blink);
                Some(0)
            }
        }
        SMI_DOCK_DISCONNECT => {
            let handshake = h8.dock_latch(false);
            dock::dock_disconnect_with_ec(ec);
            Some(u8::from(handshake))
        }
        _ => None,
    }
}

fn handle_ec_event() {
    // Under ACPI the OS owns EC queries; SMM must not steal its events.
    if smi().acpi_enabled() {
        return;
    }
    if let Some(event) = Ec::new(crate::config::X61_H8.resources.os).query_event() {
        if let Some(command) = dock::smm_event_command(event) {
            let _ = dock_command(command);
        }
    }
}

pub struct LenovoX61SmmHandler;

impl IchBoardSmmHandler for LenovoX61SmmHandler {
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
            crate::config::X61_H8.resources.smm()
        } else {
            crate::config::X61_H8.resources.os
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

fstart_platform_intel::smm::smm_bin!(Board, IchSmmHandler<LenovoX61SmmHandler>);
