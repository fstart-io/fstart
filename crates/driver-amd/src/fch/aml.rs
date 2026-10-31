// SPDX-License-Identifier: MIT OR Apache-2.0
//! ACPI devices of the FCH's MMIO controllers, as the vendors' DSDTs have
//! them; shared by every FCH (Sandstone, Taishan, SP3's).
extern crate alloc;

use alloc::vec::Vec;
use fstart_acpi::aml_linker::device_vec;
use fstart_acpi::{AmlError, acpi_dsl};

/// The GPIO banks in the ACPI MMIO.
pub const GPIO_BASE: u32 = super::ACPI_MMIO + 0x1500;
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
}
