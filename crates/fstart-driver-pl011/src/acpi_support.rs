//! ACPI table generation for the PL011 UART driver.
//!
//! Implements [`AcpiDevice`] to contribute a DSDT device node (`_HID`
//! "ARMH0011") and an SPCR (Serial Port Console Redirection) table.

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;

use fstart_acpi::aml::{Device, Interrupt, Memory32Fixed, Name, ResourceTemplate};
use fstart_acpi::device::AcpiDevice;
use fstart_acpi::Aml;

use crate::{Pl011, Pl011Config};

impl AcpiDevice for Pl011 {
    type Config = Pl011Config;

    fn dsdt_aml(&self, config: &Pl011Config) -> Vec<u8> {
        let name = config.acpi_name.as_deref().unwrap_or("COM0");
        let gsiv = config.acpi_gsiv.unwrap_or(0);

        let irq = Interrupt::new(true, false, false, false, gsiv);
        let mmio = Memory32Fixed::new(true, config.base_addr as u32, 0x1000);
        let crs = ResourceTemplate::new(vec![&mmio, &irq]);

        let hid_val: &str = "ARMH0011";
        let hid = Name::new("_HID".into(), &hid_val);
        let uid = Name::new("_UID".into(), &0u32);
        let crs_name = Name::new("_CRS".into(), &crs);

        let dev = Device::new(name.into(), vec![&hid, &uid, &crs_name]);

        let mut bytes = Vec::new();
        dev.to_aml_bytes(&mut bytes);
        bytes
    }

    fn extra_tables(&self, config: &Pl011Config) -> Vec<Vec<u8>> {
        let gsiv = config.acpi_gsiv.unwrap_or(0);
        let spcr = fstart_acpi::spcr::build_spcr_pl011(config.base_addr, gsiv);
        let mut bytes = Vec::new();
        spcr.to_aml_bytes(&mut bytes);
        vec![bytes]
    }
}
