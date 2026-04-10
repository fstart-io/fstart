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

use acpi_tables::aml::{
    AddressSpace, AddressSpaceCacheable, Device, Interrupt, Memory32Fixed, Name, ResourceTemplate,
    IO,
};
use acpi_tables::mcfg::MCFG;
use acpi_tables::Aml;

use crate::serialize;

// ---------------------------------------------------------------------------
// Auto-width MMIO descriptor — picks 32-bit vs 64-bit based on address range
// ---------------------------------------------------------------------------

/// MMIO resource descriptor with automatic width selection.
///
/// Mirrors coreboot's `acpigen_resource_mmio()` pattern: if the entire
/// region fits below 4 GiB, emit a compact `Memory32Fixed` descriptor;
/// otherwise emit a `QWordMemory` address space descriptor.
enum MmioDescriptor {
    /// 32-bit fixed memory range (ACPI tag `0x86`).
    Fixed32(Memory32Fixed),
    /// 64-bit memory address space (ACPI tag `0x8A`).
    QWord(AddressSpace<u64>),
}

impl MmioDescriptor {
    /// Create an MMIO descriptor for the given base address and size.
    ///
    /// Uses `Memory32Fixed` when the region fits entirely below 4 GiB,
    /// `QWordMemory` otherwise.
    fn new(base: u64, size: u64) -> Self {
        let end = base.saturating_add(size).saturating_sub(1);
        if end < (1u64 << 32) {
            MmioDescriptor::Fixed32(Memory32Fixed::new(true, base as u32, size as u32))
        } else {
            MmioDescriptor::QWord(AddressSpace::<u64>::new_memory(
                AddressSpaceCacheable::NotCacheable,
                true,
                base,
                end,
                None,
            ))
        }
    }
}

impl Aml for MmioDescriptor {
    fn to_aml_bytes(&self, sink: &mut dyn acpi_tables::AmlSink) {
        match self {
            Self::Fixed32(m) => m.to_aml_bytes(sink),
            Self::QWord(q) => q.to_aml_bytes(sink),
        }
    }
}

// ---------------------------------------------------------------------------
// GenericAcpi — multiple MMIO/PIO regions + optional interrupt
// ---------------------------------------------------------------------------

/// A generic device for ACPI table generation.
///
/// Produces a DSDT device node with `_HID`, `_UID`, and `_CRS` containing
/// one or more MMIO/PIO resource descriptors and an optional extended
/// interrupt.
///
/// MMIO regions above 4 GiB automatically use a `QWordMemory` descriptor;
/// regions that fit below 4 GiB use a compact `Memory32Fixed` descriptor.
pub struct GenericAcpi<'a> {
    /// ACPI namespace name (e.g., "DEV0").
    pub name: &'a str,
    /// ACPI `_HID` value (e.g., "ACPI0007").
    pub hid: &'a str,
    /// Hardware resources (MMIO regions, Port I/O ranges).
    pub resources: &'a [fstart_types::acpi::AcpiResource],
    /// Interrupt GSIV (optional).
    pub gsiv: Option<u32>,
}

impl GenericAcpi<'_> {
    /// Produce AML bytes for this device's DSDT entry.
    pub fn dsdt_aml(&self) -> Vec<u8> {
        let hid_str: String = String::from(self.hid);
        let hid = Name::new("_HID".into(), &hid_str);
        let uid = Name::new("_UID".into(), &0u32);

        // Build resource descriptors for each MMIO/PIO region.
        let mut mmio_descs: Vec<MmioDescriptor> = Vec::new();
        let mut pio_descs: Vec<IO> = Vec::new();

        for res in self.resources {
            match *res {
                fstart_types::acpi::AcpiResource::Mmio { base, size } => {
                    mmio_descs.push(MmioDescriptor::new(base, size));
                }
                fstart_types::acpi::AcpiResource::Pio { base, size } => {
                    pio_descs.push(IO::new(base, base, 0, size as u8));
                }
            }
        }

        // Collect all resource references for the ResourceTemplate.
        let mut crs_children: Vec<&dyn Aml> = Vec::new();
        for m in &mmio_descs {
            crs_children.push(m);
        }
        for p in &pio_descs {
            crs_children.push(p);
        }

        let irq;
        if let Some(gsiv) = self.gsiv {
            irq = Interrupt::new(true, false, false, false, gsiv);
            crs_children.push(&irq);
        }

        let crs = ResourceTemplate::new(crs_children);
        let crs_name = Name::new("_CRS".into(), &crs);
        let dev = Device::new(self.name.into(), vec![&hid, &uid, &crs_name]);

        let mut bytes = Vec::new();
        dev.to_aml_bytes(&mut bytes);
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
    ///
    /// Selects `memory_32_fixed` or `qword_memory` depending on whether
    /// the MMIO base address fits within 32 bits.
    pub fn dsdt_aml(&self) -> Vec<u8> {
        let name = self.name;
        let gsiv = self.gsiv;
        if self.base + self.size as u64 <= u32::MAX as u64 {
            let base = self.base as u32;
            let size = self.size;
            fstart_acpi_macros::acpi_dsl! {
                Device(#{name}) {
                    Name("_HID", "LNRO0015");
                    Name("_UID", 0u32);
                    Name("_CCA", 1u32);
                    Name("_CLS", Package(0x01u8, 0x06u8, 0x01u8));
                    Name("_CRS", ResourceTemplate {
                        Memory32Fixed(ReadWrite, #{base}, #{size});
                        Interrupt(ResourceConsumer, Level, ActiveHigh, Exclusive, #{gsiv});
                    });
                }
            }
        } else {
            let base = self.base;
            let end = self.base + self.size as u64 - 1;
            fstart_acpi_macros::acpi_dsl! {
                Device(#{name}) {
                    Name("_HID", "LNRO0015");
                    Name("_UID", 0u32);
                    Name("_CCA", 1u32);
                    Name("_CLS", Package(0x01u8, 0x06u8, 0x01u8));
                    Name("_CRS", ResourceTemplate {
                        QWordMemory(NotCacheable, ReadWrite, #{base}, #{end});
                        Interrupt(ResourceConsumer, Level, ActiveHigh, Exclusive, #{gsiv});
                    });
                }
            }
        }
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
        let name = self.name;
        let base = self.base;
        let size = self.size;
        let gsiv = self.gsiv;
        fstart_acpi_macros::acpi_dsl! {
            Device(#{name}) {
                Name("_HID", "PNP0D10");
                Name("_UID", 0u32);
                Name("_CCA", 1u32);
                Name("_CRS", ResourceTemplate {
                    Memory32Fixed(ReadWrite, #{base}, #{size});
                    Interrupt(ResourceConsumer, Level, ActiveHigh, Exclusive, #{gsiv});
                });
            }
        }
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
    ///
    /// Generates a complete PCIe root bridge with:
    /// - `_HID` PNP0A08 (PCIe), `_CID` PNP0A03 (PCI)
    /// - `_SEG`, `_BBN`, `_CCA`, `_UID`
    /// - `_CRS` with WordBusNumber, DWordMemory (32-bit MMIO),
    ///   QWordMemory (64-bit MMIO), and optionally INTA-INTD interrupts
    /// - `_OSC` method (accepts all OS-requested capabilities)
    pub fn dsdt_aml(&self) -> Vec<u8> {
        let name = self.name;
        let seg = self.segment as u32;
        let bbn = self.bus_start as u32;
        let bus_start = self.bus_start;
        let bus_end = self.bus_end;
        let mmio32_base = self.mmio32_base;
        let mmio32_end = self.mmio32_end;
        let mmio64_base = self.mmio64_base;
        let mmio64_end = self.mmio64_end;
        let irq_a = self.irqs[0];
        let irq_b = self.irqs[1];
        let irq_c = self.irqs[2];
        let irq_d = self.irqs[3];
        fstart_acpi_macros::acpi_dsl! {
            Device(#{name}) {
                Name("_HID", EisaId("PNP0A08"));
                Name("_CID", EisaId("PNP0A03"));
                Name("_SEG", #{seg});
                Name("_BBN", #{bbn});
                Name("_CCA", 1u32);
                Name("_UID", 0u32);
                Name("_CRS", ResourceTemplate {
                    WordBusNumber(#{bus_start}, #{bus_end});
                    DWordMemory(NotCacheable, ReadWrite, #{mmio32_base}, #{mmio32_end});
                    QWordMemory(NotCacheable, ReadWrite, #{mmio64_base}, #{mmio64_end});
                    Interrupt(ResourceConsumer, Level, ActiveHigh, Exclusive, #{irq_a});
                    Interrupt(ResourceConsumer, Level, ActiveHigh, Exclusive, #{irq_b});
                    Interrupt(ResourceConsumer, Level, ActiveHigh, Exclusive, #{irq_c});
                    Interrupt(ResourceConsumer, Level, ActiveHigh, Exclusive, #{irq_d});
                });
                Method("_OSC", 4, NotSerialized) {
                    Return(#{acpi_tables::aml::Arg(3)});
                }
            }
        }
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
        use fstart_types::acpi::AcpiResource;

        let resources = [AcpiResource::Mmio {
            base: 0x6001_0000,
            size: 0x1000,
        }];
        let dev = GenericAcpi {
            name: "DEV0",
            hid: "ACPI0007",
            resources: &resources,
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
        use fstart_types::acpi::AcpiResource;

        let resources = [AcpiResource::Mmio {
            base: 0x7000_0000,
            size: 0x100,
        }];
        let dev = GenericAcpi {
            name: "DEV1",
            hid: "TEST0001",
            resources: &resources,
            gsiv: None,
        };
        let aml = dev.dsdt_aml();
        assert!(aml.len() > 10);
        assert_eq!(aml[0], 0x5B);
        assert_eq!(aml[1], 0x82);
    }

    #[test]
    fn test_generic_device_mixed_mmio_pio() {
        use fstart_types::acpi::AcpiResource;

        let resources = [
            AcpiResource::Mmio {
                base: 0x6001_0000,
                size: 0x1000,
            },
            AcpiResource::Pio {
                base: 0x3F8,
                size: 8,
            },
        ];
        let dev = GenericAcpi {
            name: "DEV2",
            hid: "TEST0002",
            resources: &resources,
            gsiv: Some(4),
        };
        let aml = dev.dsdt_aml();
        assert!(aml.len() > 10);
        assert_eq!(aml[0], 0x5B);
        assert_eq!(aml[1], 0x82);
    }

    #[test]
    fn test_ahci_above_4g_uses_qword() {
        let dev = AhciAcpi {
            name: "AHC0",
            base: 0x1_0006_0000_0000, // above 4 GiB
            size: 0x10000,
            gsiv: 42,
        };
        let aml = dev.dsdt_aml();
        assert!(aml.len() > 10);
        // Should contain QWordMemory descriptor tag 0x8A somewhere
        assert!(
            aml.windows(2).any(|w| w == [0x8A, 0x2B]),
            "expected QWordMemory descriptor (0x8A) for above-4G address"
        );
    }

    #[test]
    fn test_ahci_below_4g_uses_memory32fixed() {
        let dev = AhciAcpi {
            name: "AHC0",
            base: 0x6010_0000, // below 4 GiB
            size: 0x10000,
            gsiv: 42,
        };
        let aml = dev.dsdt_aml();
        assert!(aml.len() > 10);
        // Should contain Memory32Fixed descriptor tag 0x86
        assert!(
            aml.contains(&0x86),
            "expected Memory32Fixed descriptor (0x86) for below-4G address"
        );
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

        // AML should contain WordBusNumber descriptor (tag 0x88)
        // for the bus range 0-255.
        assert!(
            aml.contains(&0x88),
            "expected WordBusNumber descriptor (0x88) for bus range"
        );

        // AML should contain DWordMemory descriptor (tag 0x87)
        // for the 32-bit MMIO window.
        assert!(
            aml.contains(&0x87),
            "expected DWordMemory descriptor (0x87) for 32-bit MMIO window"
        );

        // AML should contain QWordMemory descriptor (tag 0x8A)
        // for the 64-bit MMIO window.
        assert!(
            aml.windows(2).any(|w| w == [0x8A, 0x2B]),
            "expected QWordMemory descriptor (0x8A) for 64-bit MMIO window"
        );

        // AML should contain the _OSC method (MethodOp = 0x14,
        // followed by name "_OSC").
        assert!(
            aml.windows(4).any(|w| w == b"_OSC"),
            "expected _OSC method in PCI root bridge"
        );

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
