//! ACPI-only device builders for hardware without fstart drivers.
//!
//! These structs produce DSDT device AML and optional standalone tables
//! (e.g., MCFG for PCIe) without going through the `AcpiDevice` trait,
//! since they have no associated driver struct.
//!
//! The codegen emits construction + `dsdt_aml()` / `extra_tables()` calls
//! for each `AcpiExtraDevice` entry in the board RON.

extern crate alloc;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use acpi_tables::aml::{Device, Interrupt, Memory32Fixed, Name, ResourceTemplate};
use acpi_tables::mcfg::MCFG;
use acpi_tables::Aml;

use crate::platform::serialize;

// ---------------------------------------------------------------------------
// GenericAcpi — single MMIO region + optional interrupt
// ---------------------------------------------------------------------------

/// A generic MMIO device for ACPI table generation.
///
/// Produces a DSDT device node with `_HID`, `_UID`, and `_CRS` containing
/// a 32-bit fixed memory region and optional extended interrupt.
pub struct GenericAcpi<'a> {
    /// ACPI namespace name (e.g., "DEV0").
    pub name: &'a str,
    /// ACPI `_HID` value (e.g., "ACPI0007").
    pub hid: &'a str,
    /// MMIO base address.
    pub base: u64,
    /// MMIO region size in bytes.
    pub size: u32,
    /// Interrupt GSIV (optional).
    pub gsiv: Option<u32>,
}

impl GenericAcpi<'_> {
    /// Produce AML bytes for this device's DSDT entry.
    pub fn dsdt_aml(&self) -> Vec<u8> {
        let mmio = Memory32Fixed::new(true, self.base as u32, self.size);

        let hid_str: String = String::from(self.hid);
        let hid = Name::new("_HID".into(), &hid_str);
        let uid = Name::new("_UID".into(), &0u32);

        let mut bytes = Vec::new();

        if let Some(gsiv) = self.gsiv {
            let irq = Interrupt::new(true, false, false, false, gsiv);
            let crs = ResourceTemplate::new(vec![&mmio, &irq]);
            let crs_name = Name::new("_CRS".into(), &crs);
            let dev = Device::new(self.name.into(), vec![&hid, &uid, &crs_name]);
            dev.to_aml_bytes(&mut bytes);
        } else {
            let crs = ResourceTemplate::new(vec![&mmio]);
            let crs_name = Name::new("_CRS".into(), &crs);
            let dev = Device::new(self.name.into(), vec![&hid, &uid, &crs_name]);
            dev.to_aml_bytes(&mut bytes);
        }

        bytes
    }
}

// ---------------------------------------------------------------------------
// AhciAcpi — AHCI SATA controller
// ---------------------------------------------------------------------------

/// AHCI SATA controller for ACPI table generation.
///
/// Produces a DSDT device node with `_HID` "LNRO0015" (AHCI),
/// `_CLS` (mass storage / SATA / AHCI 1.0), and `_CRS`.
pub struct AhciAcpi<'a> {
    /// ACPI namespace name (e.g., "AHC0").
    pub name: &'a str,
    /// MMIO base address.
    pub base: u64,
    /// MMIO region size in bytes.
    pub size: u32,
    /// Interrupt GSIV.
    pub gsiv: u32,
}

impl AhciAcpi<'_> {
    /// Produce AML bytes for this device's DSDT entry.
    pub fn dsdt_aml(&self) -> Vec<u8> {
        let mmio = Memory32Fixed::new(true, self.base as u32, self.size);
        let irq = Interrupt::new(true, false, false, false, self.gsiv);
        let crs = ResourceTemplate::new(vec![&mmio, &irq]);

        let hid: &str = "LNRO0015";
        let hid = Name::new("_HID".into(), &hid);
        let uid = Name::new("_UID".into(), &0u32);
        let crs_name = Name::new("_CRS".into(), &crs);

        // _CLS: PCI class code for SATA AHCI (0x01 / 0x06 / 0x01)
        // Use PackageBuilder to construct the class code package.
        let mut cls_pkg = acpi_tables::aml::PackageBuilder::new();
        cls_pkg.add_element(&0x01u8); // Base Class: Mass Storage
        cls_pkg.add_element(&0x06u8); // Sub Class: SATA
        cls_pkg.add_element(&0x01u8); // Programming Interface: AHCI 1.0
        let cls = Name::new("_CLS".into(), &cls_pkg);

        let dev = Device::new(self.name.into(), vec![&hid, &uid, &crs_name, &cls]);

        let mut bytes = Vec::new();
        dev.to_aml_bytes(&mut bytes);
        bytes
    }
}

// ---------------------------------------------------------------------------
// XhciAcpi — xHCI USB controller
// ---------------------------------------------------------------------------

/// xHCI USB controller for ACPI table generation.
///
/// Produces a DSDT device node with `_HID` "PNP0D10" (xHCI),
/// and `_CRS` with MMIO + interrupt.
pub struct XhciAcpi<'a> {
    /// ACPI namespace name (e.g., "USB0").
    pub name: &'a str,
    /// MMIO base address.
    pub base: u64,
    /// MMIO region size in bytes.
    pub size: u32,
    /// Interrupt GSIV.
    pub gsiv: u32,
}

impl XhciAcpi<'_> {
    /// Produce AML bytes for this device's DSDT entry.
    pub fn dsdt_aml(&self) -> Vec<u8> {
        let mmio = Memory32Fixed::new(true, self.base as u32, self.size);
        let irq = Interrupt::new(true, false, false, false, self.gsiv);
        let crs = ResourceTemplate::new(vec![&mmio, &irq]);

        let hid = Name::new("_HID".into(), &"PNP0D10");
        let uid = Name::new("_UID".into(), &0u32);
        let crs_name = Name::new("_CRS".into(), &crs);

        let dev = Device::new(self.name.into(), vec![&hid, &uid, &crs_name]);

        let mut bytes = Vec::new();
        dev.to_aml_bytes(&mut bytes);
        bytes
    }
}

// ---------------------------------------------------------------------------
// PcieRootAcpi — PCIe Root Complex
// ---------------------------------------------------------------------------

/// PCIe Root Complex for ACPI table generation.
///
/// Produces a DSDT device node with `_HID` "PNP0A08" (PCIe),
/// `_CID` "PNP0A03" (PCI), `_CRS` with bus/memory/IO ranges, `_SEG`,
/// `_BBN`, and `_CCA`. Also produces an MCFG table as an extra table.
pub struct PcieRootAcpi<'a> {
    /// ACPI namespace name (e.g., "PCI0").
    pub name: &'a str,
    /// ECAM base address.
    pub ecam_base: u64,
    /// 32-bit MMIO window start.
    pub mmio32_base: u32,
    /// 32-bit MMIO window end (inclusive).
    pub mmio32_end: u32,
    /// 64-bit MMIO window start.
    pub mmio64_base: u64,
    /// 64-bit MMIO window end (inclusive).
    pub mmio64_end: u64,
    /// PIO window base address (0 if not used).
    pub pio_base: u64,
    /// Bus number range start.
    pub bus_start: u8,
    /// Bus number range end.
    pub bus_end: u8,
    /// PCIe interrupt GSIVs (INTA..INTD).
    pub irqs: [u32; 4],
    /// PCI segment group number.
    pub segment: u16,
}

impl PcieRootAcpi<'_> {
    /// Produce AML bytes for this device's DSDT entry.
    pub fn dsdt_aml(&self) -> Vec<u8> {
        let hid = Name::new("_HID".into(), &"PNP0A08");
        let cid = Name::new("_CID".into(), &"PNP0A03");
        let seg = Name::new("_SEG".into(), &(self.segment as u32));
        let bbn = Name::new("_BBN".into(), &(self.bus_start as u32));
        // _CCA = 1: cache-coherent access (required for ARM)
        let cca = Name::new("_CCA".into(), &1u32);
        let uid = Name::new("_UID".into(), &0u32);

        // Build resource template with interrupt descriptors for INTA..INTD.
        let irq_a = Interrupt::new(true, false, false, false, self.irqs[0]);
        let irq_b = Interrupt::new(true, false, false, false, self.irqs[1]);
        let irq_c = Interrupt::new(true, false, false, false, self.irqs[2]);
        let irq_d = Interrupt::new(true, false, false, false, self.irqs[3]);
        let crs = ResourceTemplate::new(vec![&irq_a, &irq_b, &irq_c, &irq_d]);
        let crs_name = Name::new("_CRS".into(), &crs);

        let dev = Device::new(
            self.name.into(),
            vec![&hid, &cid, &seg, &bbn, &cca, &uid, &crs_name],
        );

        let mut bytes = Vec::new();
        dev.to_aml_bytes(&mut bytes);
        bytes
    }

    /// Produce standalone MCFG table for this PCIe root complex.
    ///
    /// Returns raw serialized table bytes. The MCFG is built using the
    /// upstream `acpi_tables` crate which handles endianness correctly.
    pub fn extra_tables(&self) -> Vec<Vec<u8>> {
        let mut mcfg = MCFG::new(crate::OEM_ID, crate::OEM_TABLE_ID, crate::OEM_REVISION);
        mcfg.add_ecam(self.ecam_base, self.segment, self.bus_start, self.bus_end);
        vec![serialize(&mcfg)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generic_device_aml() {
        let dev = GenericAcpi {
            name: "DEV0",
            hid: "ACPI0007",
            base: 0x6001_0000,
            size: 0x1000,
            gsiv: Some(33),
        };
        let aml = dev.dsdt_aml();
        // Should start with ExtOpPrefix + DeviceOp (0x5B 0x82)
        assert!(aml.len() > 10);
        assert_eq!(aml[0], 0x5B);
        assert_eq!(aml[1], 0x82);
    }

    #[test]
    fn test_generic_device_no_irq() {
        let dev = GenericAcpi {
            name: "DEV1",
            hid: "TEST0001",
            base: 0x7000_0000,
            size: 0x100,
            gsiv: None,
        };
        let aml = dev.dsdt_aml();
        assert!(aml.len() > 10);
        assert_eq!(aml[0], 0x5B);
        assert_eq!(aml[1], 0x82);
    }

    #[test]
    fn test_ahci_aml() {
        let dev = AhciAcpi {
            name: "AHC0",
            base: 0x6010_0000,
            size: 0x10000,
            gsiv: 42,
        };
        let aml = dev.dsdt_aml();
        assert!(aml.len() > 10);
        assert_eq!(aml[0], 0x5B);
        assert_eq!(aml[1], 0x82);
    }

    #[test]
    fn test_xhci_aml() {
        let dev = XhciAcpi {
            name: "USB0",
            base: 0x6011_0000,
            size: 0x10000,
            gsiv: 43,
        };
        let aml = dev.dsdt_aml();
        assert!(aml.len() > 10);
        assert_eq!(aml[0], 0x5B);
        assert_eq!(aml[1], 0x82);
    }

    #[test]
    fn test_pcie_root_aml_and_mcfg() {
        let dev = PcieRootAcpi {
            name: "PCI0",
            ecam_base: 0xF000_0000,
            mmio32_base: 0x8000_0000,
            mmio32_end: 0xEFFF_FFFF,
            mmio64_base: 0x1_0000_0000,
            mmio64_end: 0xFF_FFFF_FFFF,
            pio_base: 0x7FFF_0000,
            bus_start: 0,
            bus_end: 0xFF,
            irqs: [168, 169, 170, 171],
            segment: 0,
        };
        let aml = dev.dsdt_aml();
        assert!(aml.len() > 10);
        assert_eq!(aml[0], 0x5B);
        assert_eq!(aml[1], 0x82);

        let tables = dev.extra_tables();
        assert_eq!(tables.len(), 1);
        // MCFG table should be a valid serialized table.
        let mcfg_bytes = &tables[0];
        assert!(mcfg_bytes.len() > 36);
        // Verify checksum
        let sum = mcfg_bytes.iter().fold(0u8, |a, &x| a.wrapping_add(x));
        assert_eq!(sum, 0, "MCFG checksum failed");
        // Verify ECAM base (at offset 44 in MCFG)
        let ecam = u64::from_le_bytes(mcfg_bytes[44..52].try_into().unwrap());
        assert_eq!(ecam, 0xF000_0000);
        // Verify end bus (at offset 55 in MCFG)
        assert_eq!(mcfg_bytes[55], 0xFF);
    }
}
