//! Lenovo ThinkPad X61 mainboard SMM policy.

use fstart_driver_intel::southbridge::smi::{ICH8_GPE0, IchBoardSmmHandler, IchSmi, IchSmmHandler};
use fstart_driver_lenovo::ec::Ec;
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
        Ec::H8_SMM
    } else {
        Ec::LEGACY
    }
}

fn dock_command(command: u8) -> Option<u8> {
    let ec = ec_channel();
    match command {
        SMI_DOCK_CONNECT => {
            let handshake = ec.clear_bit(0x03, 2);
            fstart_arch::udelay(250_000);
            let connected = handshake && dock::dock_connect_with_ec(ec).is_ok();
            if connected {
                let state = ec.set_bit(0x03, 2);
                let led_off = ec.write(0x0c, 0x09);
                let led_on = ec.write(0x0c, 0x88);
                Some(u8::from(state && led_off && led_on))
            } else {
                let _ = ec.write(0x0c, 0x08);
                let _ = ec.write(0x0c, 0xc9);
                Some(0)
            }
        }
        SMI_DOCK_DISCONNECT => {
            let handshake = ec.clear_bit(0x03, 2);
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
    if let Some(event) = Ec::LEGACY.query_event() {
        if let Some(command) = dock::smm_event_command(event) {
            let _ = dock_command(command);
        }
    }
}

pub struct LenovoX61SmmHandler;

impl IchBoardSmmHandler for LenovoX61SmmHandler {
    unsafe fn on_apmc(_ctx: &mut SmmContext<'_>, command: u8) {
        let acpi = match command {
            0xe1 => true,
            0x1e => false,
            _ => return,
        };
        // SAFETY: SMM rendezvous owns PCI config and decoded PM registers.
        unsafe { smi().route_gpi(EC_GPIO, acpi) };
        // Discard pending events and enable attention on the SMM-owned channel.
        let ec = if acpi { Ec::H8_SMM } else { Ec::LEGACY };
        let _ = ec.write(0x80, 0x01);
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
