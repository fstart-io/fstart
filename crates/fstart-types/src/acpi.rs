//! ACPI configuration types for board RON.
//!
//! Defines the board-level ACPI configuration: platform table parameters
//! (MADT, GTDT, FADT) and declarations for ACPI-only devices (hardware
//! without fstart driver crates).
//!
//! Per-driver ACPI fields (e.g., `acpi_name`, `acpi_gsiv`) live in each
//! driver's own `Config` struct, not here.

use heapless::String as HString;
use serde::{Deserialize, Serialize};

/// Top-level ACPI configuration, from the board RON `acpi` field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpiConfig {
    /// Physical address where ACPI tables will be placed in DRAM.
    pub table_addr: u64,

    /// Platform-specific ACPI parameters (MADT, GTDT, FADT).
    pub platform: AcpiPlatform,

    /// ACPI device entries for hardware without fstart drivers.
    ///
    /// These produce DSDT entries and optional standalone tables.
    /// Once a real fstart driver is added for a device, it should
    /// implement `AcpiDevice` and move out of this list.
    #[serde(default)]
    pub extra_devices: heapless::Vec<AcpiExtraDevice, 16>,
}

/// Platform-specific ACPI table parameters.
///
/// Each variant carries the parameters needed for platform-level tables
/// (MADT, GTDT, FADT) that describe the interrupt controller, timers,
/// and power management model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AcpiPlatform {
    /// ARM platform — GICv3, generic timers, HW-reduced ACPI with PSCI.
    ///
    /// Applicable to any ARM system using GICv3 and generic timers:
    /// SBSA servers, QEMU virt, real hardware.
    Arm(ArmPlatformAcpi),
}

/// ARM platform ACPI parameters.
///
/// Describes the GICv3 interrupt controller, ARM generic timer, and
/// optional SBSA watchdog for MADT, GTDT, and FADT generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArmPlatformAcpi {
    /// Number of CPUs.
    pub num_cpus: u32,
    /// GIC Distributor base address.
    pub gic_dist_base: u64,
    /// GIC Redistributor base address.
    pub gic_redist_base: u64,
    /// GIC Redistributor discovery range length in bytes.
    ///
    /// If `None`, defaults to `num_cpus * 0x20000` (two 64 KiB frames
    /// per CPU for GICv3). Set explicitly when the hardware maps a
    /// larger region than strictly needed (e.g., QEMU SBSA-ref maps
    /// 64 MiB regardless of CPU count).
    #[serde(default)]
    pub gic_redist_length: Option<u32>,
    /// GIC Interrupt Translation Service (ITS) base address.
    ///
    /// Required for MSI/MSI-X support with PCIe devices on GICv3.
    /// If `None`, no GIC ITS subtable is added to the MADT.
    #[serde(default)]
    pub gic_its_base: Option<u64>,
    /// Timer GSIVs: (secure_el1, nonsecure_el1, virtual, nonsecure_el2).
    pub timer_gsivs: (u32, u32, u32, u32),
    /// SBSA Generic Watchdog (optional).
    #[serde(default)]
    pub watchdog: Option<AcpiWatchdog>,
    /// IORT (IO Remapping Table) configuration.
    ///
    /// Required when PCIe devices use MSI/MSI-X through the GIC ITS.
    /// Maps PCI Request IDs to GIC ITS device IDs.
    #[serde(default)]
    pub iort: Option<AcpiIort>,
}

/// SBSA Generic Watchdog parameters for GTDT.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct AcpiWatchdog {
    /// Refresh frame base address.
    pub refresh_base: u64,
    /// Control frame base address.
    pub control_base: u64,
    /// Watchdog GSIV.
    pub gsiv: u32,
}

/// IORT (IO Remapping Table) configuration.
///
/// Describes the mapping between PCI Request IDs and GIC ITS device IDs.
/// Without IORT, PCIe MSI/MSI-X cannot be routed through the GIC ITS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpiIort {
    /// GIC ITS identifiers (must match MADT GIC ITS entries).
    ///
    /// Typically `[0]` for a single-ITS system.
    pub its_ids: heapless::Vec<u32, 8>,
    /// PCI segment number (usually 0).
    #[serde(default)]
    pub pci_segment: u32,
    /// Memory address size limit in bits (e.g., 48 for 256 TiB).
    #[serde(default = "default_memory_address_limit")]
    pub memory_address_limit: u8,
    /// Number of PCI Request IDs to map (e.g., 0x10000 for full 16-bit range).
    #[serde(default = "default_id_count")]
    pub id_count: u32,
}

fn default_memory_address_limit() -> u8 {
    48
}

fn default_id_count() -> u32 {
    0x10000
}

// ---------------------------------------------------------------------------
// ACPI-only devices (no fstart driver crate)
// ---------------------------------------------------------------------------

/// An ACPI device that has no fstart driver crate.
///
/// These entries produce DSDT device nodes and optional standalone
/// tables.  Each variant carries the minimum hardware parameters
/// needed for ACPI table generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AcpiExtraDevice {
    /// Generic MMIO device with a single MMIO region and interrupt.
    Generic(AcpiGenericDevice),
    /// AHCI SATA controller.
    Ahci(AcpiAhciDevice),
    /// xHCI USB controller.
    Xhci(AcpiXhciDevice),
    /// PCIe Root Complex.
    PcieRoot(AcpiPcieRootDevice),
}

/// A generic MMIO device for ACPI (single region + interrupt).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpiGenericDevice {
    /// ACPI namespace name (e.g., "DEV0").
    pub name: HString<8>,
    /// ACPI `_HID` value (e.g., "ACPI0007").
    pub hid: HString<16>,
    /// MMIO base address.
    pub base: u64,
    /// MMIO region size in bytes.
    pub size: u32,
    /// Interrupt GSIV (optional).
    #[serde(default)]
    pub gsiv: Option<u32>,
}

/// AHCI SATA controller for ACPI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpiAhciDevice {
    /// ACPI namespace name (e.g., "AHC0").
    pub name: HString<8>,
    /// MMIO base address.
    pub base: u64,
    /// MMIO region size in bytes.
    pub size: u32,
    /// Interrupt GSIV.
    pub gsiv: u32,
}

/// xHCI USB controller for ACPI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpiXhciDevice {
    /// ACPI namespace name (e.g., "USB0").
    pub name: HString<8>,
    /// MMIO base address.
    pub base: u64,
    /// MMIO region size in bytes.
    pub size: u32,
    /// Interrupt GSIV.
    pub gsiv: u32,
}

/// PCIe Root Complex for ACPI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpiPcieRootDevice {
    /// ACPI namespace name (e.g., "PCI0").
    pub name: HString<8>,
    /// ECAM base address.
    pub ecam_base: u64,
    /// 32-bit MMIO window (start, end inclusive).
    pub mmio32: (u32, u32),
    /// 64-bit MMIO window (start, end inclusive).
    pub mmio64: (u64, u64),
    /// PIO window base address.
    #[serde(default)]
    pub pio_base: Option<u64>,
    /// Bus number range (start, end).
    #[serde(default = "default_bus_range")]
    pub bus_range: (u8, u8),
    /// PCIe interrupt GSIVs (INTA..INTD).
    pub irqs: [u32; 4],
    /// PCI segment group number.
    #[serde(default)]
    pub segment: u16,
}

fn default_bus_range() -> (u8, u8) {
    (0, 0xFF)
}
