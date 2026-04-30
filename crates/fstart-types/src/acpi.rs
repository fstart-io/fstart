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
    /// Platform-specific ACPI parameters (MADT, GTDT, FADT).
    pub platform: AcpiPlatform,
}

/// Platform-specific ACPI table parameters.
///
/// Each variant carries the parameters needed for platform-level tables
/// (MADT, GTDT/HPET, FADT) that describe the interrupt controller,
/// timers, and power management model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AcpiPlatform {
    /// ARM platform -- GICv3, generic timers, HW-reduced ACPI with PSCI.
    ///
    /// Applicable to any ARM system using GICv3 and generic timers:
    /// SBSA servers, QEMU virt, real hardware.
    Arm(ArmPlatformAcpi),

    /// x86 platform -- Local APIC + I/O APIC, optional HPET.
    ///
    /// Applicable to any x86/x86_64 system with APIC interrupt
    /// controller.  Supports both legacy (8259 PIC) and modern
    /// (HW-reduced) configurations.
    X86(X86PlatformAcpi),
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

/// A hardware resource for ACPI `_CRS` generation.
///
/// Follows coreboot's resource model: each device can have multiple
/// MMIO regions and/or Port I/O ranges, each described separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AcpiResource {
    /// Memory-mapped I/O region.
    ///
    /// Addresses above 4 GiB automatically use a QWordMemory descriptor;
    /// addresses below 4 GiB use a compact Memory32Fixed descriptor.
    Mmio {
        /// Physical base address of the MMIO region.
        base: u64,
        /// Region size in bytes.
        size: u64,
    },
    /// Port I/O range.
    ///
    /// Uses the ACPI I/O Port Descriptor (16-bit address space).
    Pio {
        /// Base I/O port address.
        base: u16,
        /// Number of I/O ports.
        size: u16,
    },
}

/// A generic device for ACPI — multiple MMIO/PIO regions + optional interrupt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpiGenericDevice {
    /// ACPI namespace name (e.g., "DEV0").
    pub name: HString<8>,
    /// ACPI `_HID` value (e.g., "ACPI0007").
    pub hid: HString<16>,
    /// Hardware resources (MMIO regions, Port I/O ranges).
    pub resources: heapless::Vec<AcpiResource, 8>,
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

// ---------------------------------------------------------------------------
// x86 platform ACPI types
// ---------------------------------------------------------------------------

/// x86 platform ACPI parameters.
///
/// Describes the APIC interrupt controller, optional HPET, and
/// boot configuration for MADT, HPET, and FADT generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct X86PlatformAcpi {
    /// Number of CPUs.
    ///
    /// When `None`, the MADT builder enumerates LAPIC IDs at runtime via
    /// CPUID leaf 0x0B (x2APIC topology). This is essential for
    /// multi-board images where CPU counts differ per board.
    #[serde(default)]
    pub num_cpus: Option<u32>,
    /// Local APIC base address (usually `0xFEE0_0000`).
    #[serde(default = "default_lapic_base")]
    pub lapic_base: u64,
    /// I/O APIC entries.
    pub ioapics: heapless::Vec<IoApicEntry, 4>,
    /// Interrupt Source Override entries (ISA IRQ remapping).
    ///
    /// The most common override maps ISA IRQ 0 (PIT timer) to GSI 2.
    #[serde(default)]
    pub isos: heapless::Vec<IsoEntry, 16>,
    /// HPET base address (optional).
    ///
    /// If `None`, the platform uses the PM Timer from FADT instead.
    #[serde(default)]
    pub hpet_base: Option<u64>,
    /// Whether legacy devices (8259 PIC, ISA bus) are present.
    ///
    /// Controls the MADT `PCAT_COMPAT` flag and FADT legacy fields.
    #[serde(default)]
    pub legacy_devices: bool,
    /// SCI interrupt number (System Control Interrupt for ACPI events).
    #[serde(default = "default_sci_irq")]
    pub sci_irq: u8,
    /// PMBASE I/O port base (chipset-specific, e.g. 0x500 for ICH7).
    ///
    /// Used by FADT to derive PM1a_EVT_BLK, PM1a_CNT_BLK, PM_TMR_BLK,
    /// and GPE0_BLK register addresses.
    #[serde(default = "default_pmbase")]
    pub pmbase: u16,
}

/// I/O APIC configuration for x86 MADT.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct IoApicEntry {
    /// I/O APIC ID.
    pub id: u8,
    /// Memory-mapped base address.
    pub base: u64,
    /// Global System Interrupt base (first GSI handled by this I/O APIC).
    pub gsi_base: u32,
}

/// Interrupt Source Override (ISO) for x86 MADT.
///
/// Maps an ISA interrupt to a different GSI with specified
/// trigger/polarity settings.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct IsoEntry {
    /// Bus source (0 = ISA).
    #[serde(default)]
    pub bus: u8,
    /// Source IRQ (ISA IRQ number).
    pub source: u8,
    /// Global System Interrupt target.
    pub gsi: u32,
    /// MPS INTI flags (trigger mode and polarity).
    #[serde(default)]
    pub flags: u16,
}

fn default_lapic_base() -> u64 {
    0xFEE0_0000
}

fn default_sci_irq() -> u8 {
    9
}

/// Default PMBASE for ICH7-era southbridges.
fn default_pmbase() -> u16 {
    0x0500
}
