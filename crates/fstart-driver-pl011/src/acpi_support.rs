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
        let mut tables = Vec::new();

        // SPCR: Serial Port Console Redirection.
        let spcr = fstart_acpi::spcr::build_spcr_pl011(config.base_addr, gsiv);
        let mut spcr_bytes = Vec::new();
        spcr.to_aml_bytes(&mut spcr_bytes);
        tables.push(spcr_bytes);

        // DBG2: Debug Port Table 2 (optional, for SBSA compliance).
        if config.acpi_dbg2 {
            let acpi_name = config.acpi_name.as_deref().unwrap_or("COM0");
            let mut namespace = heapless::String::<32>::new();
            // Build namespace path: "\\_SB.<acpi_name>"
            let _ = namespace.push_str("\\_SB.");
            let _ = namespace.push_str(acpi_name);

            let dbg2 = fstart_acpi::dbg2::build_dbg2_pl011(&fstart_acpi::dbg2::Dbg2Pl011Config {
                base_addr: config.base_addr,
                addr_size: 0x1000, // PL011 MMIO region size
                namespace: namespace.as_str(),
            });
            let mut dbg2_bytes = Vec::new();
            dbg2.to_aml_bytes(&mut dbg2_bytes);
            tables.push(dbg2_bytes);
        }

        tables
    }
}
