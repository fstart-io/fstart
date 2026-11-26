// SPDX-License-Identifier: MIT OR Apache-2.0
//! A Qilian FCH UART as an SP7 board's console (DEVICES.md §2.5): the
//! board picks the UART ([`FchUart`]), prepares it before the console
//! comes up ([`FchUart::prepare`]: power, pads, the interrupt route the
//! HW-ACPI env writes anyway, an optional legacy I/O alias), logs through
//! [`FchUart::ns16550`] and publishes [`FchUart::spcr`]. The UART's ACPI
//! device (`FURn`, `AMDI0020`, or `COMn` when legacy-mapped) comes from
//! `fch::aml::FchAcpi`, which reads the UART back as powered.
//!
//! A board whose console is its BMC's Super I/O UART on eSPI (the MD25-CG0
//! and both donors) does not use this; GIGABYTE's DSDT carries the
//! `FURn` objects for the FCH UARTs all the same.

extern crate alloc;

use fstart_driver_amd::fch::qilian::espi::Decode;
use fstart_driver_amd::fch::qilian::uart::{self, LegacyPort};
use fstart_driver_amd::fch::{FchHw, irq};
use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};

/// The FCH UARTs' interrupt routes as the vendor's HW-ACPI env programs
/// them (`PCI_INTR 0xF4/0xF5/0xF8`, FCH.md §2.8).
pub const DEFAULT_IRQ: [u8; 3] = [3, 4, 3];

/// A Qilian UART as the console.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FchUart {
    /// UART 0..2.
    pub uart: u8,
    /// Its ISA IRQ in PIC and IOAPIC mode.
    pub irq: u8,
    /// A legacy I/O range to alias it at too (`FEDC_0020`); dropped when
    /// an eSPI range already decodes that port.
    pub legacy: Option<LegacyPort>,
}

/// Why a console UART was not prepared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Uart(uart::Error),
    Legacy(uart::Shared),
}

impl FchUart {
    /// UART `uart` on the vendor's IRQ, MMIO only.
    pub const fn new(uart: u8) -> Self {
        Self {
            uart,
            irq: if (uart as usize) < DEFAULT_IRQ.len() {
                DEFAULT_IRQ[uart as usize]
            } else {
                0
            },
            legacy: None,
        }
    }

    /// The console's register access: byte registers at a four-byte stride
    /// in the UART's MMIO, its 48 MHz clock. Evaluated in a constant, a
    /// UART Qilian lacks fails the build. `#[inline]`: the panic string
    /// stays out of other SoCs' images if it is ever called at run time.
    #[inline]
    pub const fn ns16550(&self, baud_rate: u32) -> Ns16550Config {
        let Some(base) = uart::base(self.uart) else {
            panic!("no such Qilian UART");
        };
        Ns16550Config {
            regs: AccessMode::Mmio {
                base,
                reg_shift: 2,
                reg_width: 1,
            },
            clock_freq: fstart_driver_amd::fch::uart::MMIO_CLOCK_HZ,
            baud_rate,
        }
    }

    /// Power the UART and give it its pads (`qilian::uart::enable`), route
    /// its interrupt in both modes, and alias it at its legacy range
    /// unless `espi` decodes that port.
    pub fn prepare(&self, fch: &impl FchHw, espi: &Decode) -> Result<(), Error> {
        uart::enable(fch, self.uart).map_err(Error::Uart)?;
        let source = uart::intr_source(self.uart).ok_or(Error::Uart(uart::Error::Uart))?;
        for mode in [irq::Mode::Pic, irq::Mode::Apic] {
            irq::write(fch.io(), irq::index(source, mode), self.irq);
        }
        if let Some(port) = self.legacy {
            let mut legacy = [None; 4];
            legacy[usize::from(self.uart)] = Some(port);
            let legacy = uart::without_espi_conflicts(legacy, espi);
            uart::set_legacy(fch, legacy).map_err(Error::Legacy)?;
        }
        Ok(())
    }

    /// The SPCR of the console: a 16550 of 32-bit registers in the UART's
    /// MMIO, polled, 115200 baud (the vendors' FURn device is the same
    /// UART; neither donor publishes a DBG2).
    pub fn spcr(&self) -> alloc::vec::Vec<u8> {
        let base = uart::base(self.uart).unwrap_or(0);
        let mut out = alloc::vec::Vec::new();
        fstart_acpi::Aml::to_aml_bytes(
            &fstart_acpi::spcr::build_spcr_16550_mmio32(
                base,
                fstart_driver_amd::fch::uart::MMIO_CLOCK_HZ,
            ),
            &mut out,
        );
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_driver_amd::fch::testing::{Space, World};

    #[test]
    fn uart1_console_is_powered_routed_and_described() {
        const CONSOLE: Ns16550Config = FchUart::new(1).ns16550(115_200);
        assert!(matches!(
            CONSOLE.regs,
            AccessMode::Mmio {
                base: 0xfedc_a000,
                ..
            }
        ));
        let world = World::default();
        // AOAC device 0x0C reports up.
        world.set(Space::Mmio, 0xfed8_0000 + 0x1e41 + 2 * 0x0c, 1, 7);
        let fch = world.fch();
        let uart = FchUart {
            legacy: Some(LegacyPort::P3f8),
            ..FchUart::new(1)
        };
        uart.prepare(&fch, &Decode::default()).unwrap();
        // UART1 behind range 3 (3F8h): bit 3 and UART 1 in bits 15:14.
        assert_eq!(world.get(Space::Mmio, 0xfedc_0020, 2), 1 << 3 | 1 << 14);
        let spcr = uart.spcr();
        assert_eq!(&spcr[..4], b"SPCR");
    }
}
