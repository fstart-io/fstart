// SPDX-License-Identifier: MIT OR Apache-2.0
//! ACPI devices of the FCH's MMIO controllers and of its ISA legacy
//! devices, as the vendors' DSDTs have them, and the FCH's ports and MMIO
//! no device claims; shared by every FCH (Sandstone, Taishan, SP3's).
extern crate alloc;

use alloc::vec::Vec;
use fstart_acpi::aml_linker::device_vec;
use fstart_acpi::{AmlError, acpi_dsl};

use super::{ACPI_MMIO, HPET, SMBUS_ASF_IO_BASE, SPI_BASE, UART_CLOCK_HZ, UART_MMIO};

/// The GPIO banks in the ACPI MMIO.
pub const GPIO_BASE: u32 = ACPI_MMIO + 0x1500;
/// Path of the GPIO controller, [`gpio`].
pub const GPIO_PATH: &str = "\\_SB.GPIO";

/// The GPIO controller `\_SB.GPIO` (`AMDI0030`): `length` bytes of GPIO
/// banks from [`GPIO_BASE`] (0x300 on Zeppelin and SP3, 0x400 on Picasso),
/// interrupting on the level-low, shared FCH IRQ `irq`.
pub fn gpio(irq: u8, length: u32) -> Result<Vec<u8>, AmlError> {
    let (irq, length) = (u64::from(irq), u64::from(length));
    let mut body = Vec::new();
    acpi_dsl! {
        Name("_HID", "AMDI0030");
        Name("_CID", "AMDI0030");
        Name("_UID", 0u8);
        Name("_CRS", ResourceTemplate {
            Interrupt(ResourceConsumer, Level, ActiveLow, Shared, #{dword irq});
            Memory32Fixed(ReadWrite, #{const dword GPIO_BASE}, #{dword length});
        });
    }
    .emit(&mut body)?;
    device_vec("GPIO", &body)
}

/// A DesignWare I2C controller `name` (`AMDI0010`) at `base`, interrupting
/// on the edge-high FCH IRQ `irq`, with `body`: its timing and the devices
/// behind it.
pub fn i2c(name: &str, uid: u8, base: u32, irq: u8, body: &[u8]) -> Result<Vec<u8>, AmlError> {
    let (uid, base, irq) = (u64::from(uid), u64::from(base), u64::from(irq));
    let mut device = Vec::new();
    acpi_dsl! {
        Name("_HID", "AMDI0010");
        Name("_UID", #{byte uid});
        Name("_CRS", ResourceTemplate {
            Interrupt(ResourceConsumer, Edge, ActiveHigh, Exclusive, #{dword irq});
            Memory32Fixed(ReadWrite, #{dword base}, 0x1000u32);
        });
    }
    .emit(&mut device)?;
    device.extend_from_slice(body);
    device_vec(name, &device)
}

/// FCH UART `index` (`AMDI0020`), `FUR0`-`FUR3`: its MMIO and DMA blocks
/// ([`UART_MMIO`]), interrupting on the edge-high FCH IRQ `irq`, with the
/// 8250 properties of its MMIO mode.
pub fn uart(index: u8, irq: u8) -> Result<Vec<u8>, AmlError> {
    let &(mmio, dma) = UART_MMIO
        .get(usize::from(index))
        .ok_or(AmlError::InvalidOperand)?;
    let (uid, irq, mmio, dma) = (
        u64::from(index),
        u64::from(irq),
        u64::from(mmio),
        u64::from(dma),
    );
    let mut body = Vec::new();
    acpi_dsl! {
        Name("_HID", "AMDI0020");
        Name("_UID", #{byte uid});
        Name("_DSD", Package(
            ToUUID("daffd814-6eba-4d8c-8a91-bc9bbf4aa301"),
            Package(
                Package("clock-frequency", #{const dword UART_CLOCK_HZ}),
                Package("reg-shift", 2u8),
                Package("reg-io-width", 1u8),
            )
        ));
        Name("_CRS", ResourceTemplate {
            Interrupt(ResourceConsumer, Edge, ActiveHigh, Exclusive, #{dword irq});
            Memory32Fixed(ReadWrite, #{dword mmio}, 0x1000u32);
            Memory32Fixed(ReadWrite, #{dword dma}, 0x1000u32);
        });
    }
    .emit(&mut body)?;
    let name = [b'F', b'U', b'R', b'0' + index];
    device_vec(
        core::str::from_utf8(&name).map_err(|_| AmlError::InvalidPath)?,
        &body,
    )
}

/// FCH I/O ports that no device object claims, `(port, length)`: the
/// legacy ranges, PCI_INTR, PMIO/SMBus index pairs, SMBus and ASF (the SP3
/// donors' `S900`, which lists the same set; 0x60/0x64 have length 0
/// there). The ACPI PM blocks are the SoC's to add.
pub const SYSTEM_IO: &[(u16, u8)] = &[
    (0x10, 0x10),
    (0x22, 0x1E),
    (0x63, 1),
    (0x65, 1),
    (0x67, 9),
    (0x72, 0x0E),
    (0x80, 1),
    (0x84, 3),
    (0x88, 1),
    (0x8C, 3),
    (0x90, 0x10),
    (0xA2, 0x1E),
    (0xB1, 1),
    (0xE0, 0x10),
    (0x40B, 1),
    (0x4D6, 1),
    (0x900, 0x10),
    (0x910, 0x10),
    // PCI_INTR index and data.
    (0xC00, 2),
    (0xC14, 1),
    (0xC50, 3),
    (0xC6C, 1),
    (0xC6F, 1),
    (0xCD0, 0x10),
    (SMBUS_ASF_IO_BASE, 0x10),
    (SMBUS_ASF_IO_BASE + 0x20, 0x20),
];

/// FCH MMIO that no device object claims, `(base, length)`: the ACPI MMIO
/// block around the GPIO banks the `GPIO` device owns, the `FEDC_0000h`
/// block, the SPI controller and the flash window (the SP3 donors' `S900`).
pub const SYSTEM_MMIO: &[(u32, u32)] = &[
    (ACPI_MMIO, GPIO_BASE - ACPI_MMIO),
    (GPIO_BASE + 0x400, 0xFED9_0000 - (GPIO_BASE + 0x400)),
    (0xFEDC_0000, 0x1000),
    (SPI_BASE, 0x1000),
    (0xFF00_0000, 0x0100_0000),
];

/// ISA legacy devices for the LPC bridge scope: PIC, DMA, PIT, RTC (no
/// IRQ 8: the HPET's legacy replacement owns it), speaker and the HPET.
pub fn lpc() -> Vec<u8> {
    acpi_dsl! {
        Device("PIC") {
            Name("_HID", EisaId("PNP0000"));
            Name("_CRS", ResourceTemplate {
                IO(0x20u16, 0x20u16, 0u8, 2u8);
                IO(0xA0u16, 0xA0u16, 0u8, 2u8);
                IO(0x4D0u16, 0x4D0u16, 0u8, 2u8);
                IRQNoFlags(2u8);
            });
        }
        Device("DMAC") {
            Name("_HID", EisaId("PNP0200"));
            Name("_CRS", ResourceTemplate {
                IO(0x00u16, 0x00u16, 0u8, 0x10u8);
                IO(0x81u16, 0x81u16, 0u8, 3u8);
                IO(0x87u16, 0x87u16, 0u8, 1u8);
                IO(0x89u16, 0x89u16, 0u8, 3u8);
                IO(0x8Fu16, 0x8Fu16, 0u8, 1u8);
                IO(0xC0u16, 0xC0u16, 0u8, 0x20u8);
            });
        }
        Device("TIMR") {
            Name("_HID", EisaId("PNP0100"));
            Name("_CRS", ResourceTemplate {
                IO(0x40u16, 0x40u16, 0u8, 4u8);
                IRQNoFlags(0u8);
            });
        }
        Device("RTC") {
            Name("_HID", EisaId("PNP0B00"));
            Name("_CRS", ResourceTemplate {
                IO(0x70u16, 0x70u16, 0u8, 2u8);
            });
        }
        Device("SPKR") {
            Name("_HID", EisaId("PNP0800"));
            Name("_CRS", ResourceTemplate {
                IO(0x61u16, 0x61u16, 0u8, 1u8);
            });
        }
        Device("HPET") {
            Name("_HID", EisaId("PNP0103"));
            Name("_UID", 0u8);
            Name("_CRS", ResourceTemplate {
                Memory32Fixed(ReadOnly, #{const dword HPET}, 0x400u32);
            });
        }
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpio_and_i2c() {
        let gpio = gpio(7, 0x300).unwrap();
        assert!(gpio.windows(8).any(|w| w == b"AMDI0030"));
        // Memory32Fixed: base 0xfed81500, length 0x300.
        let fixed = [0x86, 9, 0, 1, 0x00, 0x15, 0xd8, 0xfe, 0x00, 0x03, 0, 0];
        assert!(gpio.windows(12).any(|w| w == fixed));
        let i2c = i2c("I2CA", 0, 0xfedc_2000, 10, &[]).unwrap();
        assert!(i2c.windows(4).any(|w| w == b"I2CA"));
        assert!(i2c.windows(8).any(|w| w == b"AMDI0010"));
    }

    #[test]
    fn system_mmio_leaves_the_gpio_banks_out() {
        // The largest bank set, Picasso's.
        let gpio = u64::from(GPIO_BASE)..u64::from(GPIO_BASE + 0x400);
        assert!(SYSTEM_MMIO.iter().all(|&(b, l)| {
            let (b, l) = (u64::from(b), u64::from(l));
            b + l <= gpio.start || b >= gpio.end
        }));
        assert_eq!(
            SYSTEM_MMIO[..2],
            [(0xFED8_0000, 0x1500), (0xFED8_1900, 0xE700)]
        );
        assert!(lpc().windows(4).any(|name| name == b"HPET"));
    }
}
