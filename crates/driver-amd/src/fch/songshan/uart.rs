// SPDX-License-Identifier: MIT OR Apache-2.0
//! Songshan's MMIO UARTs: their pads and AOAC devices differ from the
//! other FCHs' ([`crate::fch::uart`]); the registers are the same.
//! Genoa openSIL and coreboot's UART, AOAC and GPIO tables.
use crate::fch::FchHw;
use crate::fch::acpimmio::{self, Error};
use crate::fch::gpio::{self, Control, Pad, Pin};
use crate::window::Window;
use embedded_hal::delay::DelayNs;
use tock_registers::interfaces::{ReadWriteable, Readable};

/// The AMBA block the UARTs sit on (AOAC device 17).
const AMBA: u8 = 17;

/// UART `n`: its MMIO, its PCI_INTR source, its AOAC device and its RX/TX
/// pads (the IOMUX function 0 of each).
const UARTS: [(u64, u8, u8, [u16; 2]); 3] = [
    (0xfedc_9000, 0x74, 11, [136, 138]),
    (0xfedc_a000, 0x75, 12, [141, 142]),
    (0xfedc_e000, 0x78, 16, [137, 135]),
];

/// The MMIO base of UART `n`.
pub const fn base(n: u8) -> u64 {
    UARTS[n as usize].0
}

/// The PCI_INTR source of UART `n`.
pub const fn intr_source(n: u8) -> u8 {
    UARTS[n as usize].1
}

/// Power the AMBA block and UART `n` and give the UART its pads, their
/// pulls off. The legacy decodes, eSPI and the other pads stay.
pub fn prepare(fch: &impl FchHw, n: u8, delay: &mut impl DelayNs) -> Result<(), Error> {
    let (_, _, device, pins) = UARTS[usize::from(n)];
    let acpi = fch.acpi();
    let control = |pin: u16| acpi.reg::<u32, Control::Register>(Pin(pin).control());
    if pins.iter().any(|&pin| control(pin).get() == u32::MAX) {
        return Err(Error::Unavailable(device));
    }
    acpimmio::power_devices(acpi, &[AMBA, device], 1000, delay)?;
    for pin in pins {
        gpio::configure(acpi, &Pad::mux(Pin(pin), 0));
        // The W1C status bits written as 0: a latched status stays.
        control(pin).modify(
            Control::PULL_UP::CLEAR
                + Control::PULL_DOWN::CLEAR
                + Control::INTERRUPT_STATUS::CLEAR
                + Control::WAKE_STATUS::CLEAR,
        );
    }
    Ok(())
}
