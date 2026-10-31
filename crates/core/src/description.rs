//! What the firmware found and set up, in one place.
//!
//! The platform phases fill a [`Description`]; the payload launchers and the
//! table exporters (the e820 map and zero page, coreboot tables, the device
//! tree, the UEFI configuration) read it instead of asking the platform for
//! the same facts through one interface per payload.

use crate::acpi::AcpiGenericDevice;
use crate::services::memory_detect::E820Entry;
use crate::services::{Console, FramebufferInfo, UartPort};

/// A physical address range.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Range {
    pub base: u64,
    pub size: u64,
}

impl Range {
    pub const fn new(base: u64, size: u64) -> Self {
        Self { base, size }
    }
}

/// Fixed identity and config-space topology of one PCI root bridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PciRootInfo {
    /// PCI segment / domain number.
    pub segment: u16,
    /// ECAM base address of the segment (bus 0), as in the MCFG: the root's
    /// buses are at `ecam_base + (bus << 20)`.
    pub ecam_base: u64,
    /// First bus number owned by this root bridge.
    pub bus_start: u8,
    /// Last bus number owned by this root bridge (inclusive).
    pub bus_end: u8,
}

impl PciRootInfo {
    /// ECAM region size derived from the inclusive bus range.
    #[must_use]
    pub const fn ecam_size(&self) -> u64 {
        if self.bus_end < self.bus_start {
            return 0;
        }
        (self.bus_end as u64 - self.bus_start as u64 + 1) * 1024 * 1024
    }
}

/// The ACPI tables the firmware built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcpiTables {
    /// RSDP physical address.
    pub rsdp: u64,
    /// The memory holding the tables and what they point to (the TCG event
    /// log, ...); reserved from the payload as ACPI NVS.
    pub region: Range,
}

impl AcpiTables {
    /// Tables in the 64 KiB from the RSDP's page.
    #[must_use]
    pub const fn at(rsdp: u64) -> Self {
        Self {
            rsdp,
            region: Range::new(rsdp & !0xfff, 0x10000),
        }
    }
}

/// The firmware's view of the machine at payload handoff.
#[derive(Clone, Copy)]
pub struct Description<'a> {
    /// The memory map after every firmware reservation.
    pub memory: &'a [E820Entry],
    /// The mounted firmware image the payload is loaded from; kept out of
    /// payload allocations.
    pub firmware: Range,
    /// More ranges the payload must not allocate (flash, firmware copies).
    pub reserved: &'a [Range],
    /// Device MMIO the payload maps (UEFI on platforms without ACPI).
    pub mmio: &'a [Range],
    /// The firmware console, for payloads that keep using it.
    pub console: Option<&'a dyn Console>,
    /// The console UART, for tables (coreboot, device tree, SPCR).
    pub uart: Option<UartPort>,
    pub pci_root: Option<PciRootInfo>,
    pub framebuffer: Option<FramebufferInfo>,
    pub acpi: Option<AcpiTables>,
    /// SMBIOS entry point physical address.
    pub smbios: Option<u64>,
    /// The device tree the platform got (from QEMU, the boot ROM or the FFS).
    pub device_tree: Option<u64>,
    /// Enabled fixed-address devices, matching the platform's ACPI tables.
    pub acpi_devices: &'a [AcpiGenericDevice],
    /// Mainboard vendor and part name.
    pub mainboard: (&'a str, &'a str),
    /// Firmware tables payloads scan (ACPI, SMBIOS, ...), reported to
    /// coreboot payloads as table memory.
    pub tables: &'a [Range],
}

impl<'a> Description<'a> {
    /// A description of `memory` with the payload loaded from `firmware`;
    /// the platform sets the rest it knows.
    #[must_use]
    pub const fn new(memory: &'a [E820Entry], firmware: Range) -> Self {
        Self {
            memory,
            firmware,
            reserved: &[],
            mmio: &[],
            console: None,
            uart: None,
            pci_root: None,
            framebuffer: None,
            acpi: None,
            smbios: None,
            device_tree: None,
            acpi_devices: &[],
            mainboard: ("", ""),
            tables: &[],
        }
    }

    /// The first RAM range: the RAM of single-bank Arm and RISC-V machines.
    #[must_use]
    pub fn ram(&self) -> Option<Range> {
        use crate::services::memory_detect::E820Kind;
        self.memory
            .iter()
            .find(|entry| entry.kind == E820Kind::Ram as u32)
            .map(|entry| Range::new(entry.addr, entry.size))
    }
}
