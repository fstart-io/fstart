// SPDX-License-Identifier: MIT OR Apache-2.0
//! Typed PCH-H pad constructors from common/block/include/intelblocks/gpio_defs.h.
//! These encode configuration only: silicon
//! reset translation, posted-write completion and ownership belong to Gpio.

use super::gpio::{Pad, PadConfig, PadDw0, PadDw1};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Pull {
    None = 0,
    Down5k = 2,
    Down20k = 4,
    Up1k = 9,
    Up5k = 10,
    Up2k = 11,
    Up20k = 12,
    Up667 = 13,
    Native = 15,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Reset {
    PowerOk = 0,
    Deep = 1,
    Platform = 2,
    Resume = 3,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum NativeFunction {
    Nf1 = 1,
    Nf2 = 2,
    Nf3 = 3,
    Nf4 = 4,
    Nf5 = 5,
    Nf6 = 6,
    Nf7 = 7,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Trigger {
    Level = 0,
    Edge = 1,
    Off = 2,
    Both = 3,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    Acpi,
    GpioDriver,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Nmi,
    Smi,
    Sci,
    IoApic,
}

/// `PAD_CFG_NF`, with IOSSTATE disabled as selected for Sunrise Point.
pub const fn native(pad: Pad, pull: Pull, reset: Reset, function: NativeFunction) -> PadConfig {
    PadConfig {
        pad,
        dw0: PadDw0::RESET.val(reset as u32).value | PadDw0::MODE.val(function as u32).value,
        dw1: PadDw1::PULL.val(pull as u32).value,
    }
}
/// `PAD_CFG_GPO`: GPIO mode, RX disabled, no pull and edge detection off.
pub const fn output(pad: Pad, high: bool, reset: Reset) -> PadConfig {
    output_with_pull(pad, high, Pull::None, reset)
}
/// `PAD_CFG_TERM_GPO`: GPIO output with explicit internal termination pull.
pub const fn output_with_pull(pad: Pad, high: bool, pull: Pull, reset: Reset) -> PadConfig {
    PadConfig {
        pad,
        dw0: PadDw0::RESET.val(reset as u32).value
            | PadDw0::TRIGGER.val(Trigger::Off as u32).value
            | PadDw0::RX_DISABLE::SET.value
            | PadDw0::TX_STATE.val(high as u32).value,
        dw1: PadDw1::PULL.val(pull as u32).value,
    }
}
/// `PAD_CFG_GPI_TRIG_OWN`; the owner bit is source metadata, not a hardware
/// DW1 bit to be written. `Gpio` consumes it when programming HOSTSW_OWN.
pub const fn input(
    pad: Pad,
    pull: Pull,
    reset: Reset,
    trigger: Trigger,
    owner: Owner,
) -> PadConfig {
    PadConfig {
        pad,
        dw0: PadDw0::RESET.val(reset as u32).value
            | PadDw0::TRIGGER.val(trigger as u32).value
            | PadDw0::TX_DISABLE::SET.value,
        dw1: PadDw1::PULL.val(pull as u32).value
            | match owner {
                Owner::Acpi => 0,
                Owner::GpioDriver => 1 << 4,
            },
    }
}
/// `PAD_CFG_GPI_APIC/SMI/SCI`: source ACPI host ownership, no invented IRQ
/// selector (the hardware DW1 INTSEL is read-only). SCI configuration remains
/// subject to the silicon driver's explicit unsupported-GPE error.
pub const fn interrupt(
    pad: Pad,
    pull: Pull,
    reset: Reset,
    trigger: Trigger,
    invert: bool,
    route: Route,
) -> PadConfig {
    let mut config = input(pad, pull, reset, trigger, Owner::Acpi);
    config.dw0 |= PadDw0::RX_INVERT.val(invert as u32).value
        | match route {
            Route::Nmi => PadDw0::ROUTE_NMI::SET.value,
            Route::Smi => PadDw0::ROUTE_SMI::SET.value,
            Route::Sci => PadDw0::ROUTE_SCI::SET.value,
            Route::IoApic => PadDw0::ROUTE_IOAPIC::SET.value,
        };
    config
}
/// `PAD_NC`: both buffers disabled, DEEP reset and no edge detection. SPT
/// does not select IOSSTATE, so do not write the later-silicon IGNORE field.
pub const fn not_connected(pad: Pad, pull: Pull) -> PadConfig {
    PadConfig {
        pad,
        dw0: PadDw0::RESET.val(Reset::Deep as u32).value
            | PadDw0::TRIGGER.val(Trigger::Off as u32).value
            | PadDw0::RX_DISABLE::SET.value
            | PadDw0::TX_DISABLE::SET.value,
        dw1: PadDw1::PULL.val(pull as u32).value,
    }
}
