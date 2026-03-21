//! SMBIOS configuration types for board RON.
//!
//! Defines the board-level SMBIOS configuration: system identity, processor
//! descriptions, and memory device declarations.  These types are deserialized
//! from the `smbios` field in the board RON and drive codegen for the
//! `SmBiosPrepare` capability.
//!
//! The actual table generation lives in the `fstart-smbios` crate.

use heapless::String as HString;
use serde::{Deserialize, Serialize};

/// Top-level SMBIOS configuration, from the board RON `smbios` field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmbiosConfig {
    /// Physical address where SMBIOS tables will be placed in DRAM.
    ///
    /// The SMBIOS 3.0 entry point is written first, followed by all
    /// structure tables.  Must be in a DRAM region accessible to the OS.
    pub table_addr: u64,

    // -- Type 0: BIOS Information --
    /// BIOS vendor string (e.g., "fstart").
    pub bios_vendor: HString<64>,
    /// BIOS version string (e.g., "0.1.0").
    pub bios_version: HString<64>,
    /// BIOS release date in MM/DD/YYYY format (e.g., "03/10/2026").
    #[serde(default = "default_bios_date")]
    pub bios_release_date: HString<16>,

    // -- Type 1: System Information --
    /// System manufacturer (e.g., "QEMU").
    pub system_manufacturer: HString<64>,
    /// System product name (e.g., "SBSA Reference").
    pub system_product: HString<64>,
    /// System version string.
    #[serde(default)]
    pub system_version: HString<64>,
    /// System serial number (optional).
    #[serde(default)]
    pub system_serial: HString<64>,

    // -- Type 2: Baseboard Information --
    /// Baseboard manufacturer.
    #[serde(default)]
    pub baseboard_manufacturer: HString<64>,
    /// Baseboard product name.
    #[serde(default)]
    pub baseboard_product: HString<64>,

    // -- Type 3: System Enclosure --
    /// Chassis / enclosure type.
    #[serde(default)]
    pub chassis_type: ChassisType,
    /// Chassis manufacturer.
    #[serde(default)]
    pub chassis_manufacturer: HString<64>,

    // -- Type 4: Processor Information --
    /// Processor descriptions (one per socket).
    #[serde(default)]
    pub processors: heapless::Vec<SmbiosProcessor, 8>,

    // -- Type 16/17/19: Memory --
    /// Memory device descriptions (one per DIMM / memory region).
    #[serde(default)]
    pub memory_devices: heapless::Vec<SmbiosMemoryDevice, 16>,
}

/// Chassis / enclosure type (SMBIOS Type 3).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChassisType {
    /// Other / unknown.
    #[default]
    Other,
    /// Desktop chassis.
    Desktop,
    /// Low-profile desktop.
    LowProfileDesktop,
    /// Tower server.
    Tower,
    /// Rack-mount server.
    RackMount,
    /// Blade server.
    Blade,
    /// Embedded / BMC.
    Embedded,
}

impl ChassisType {
    /// Convert to the SMBIOS Type 3 chassis type byte value.
    pub fn to_smbios_byte(self) -> u8 {
        match self {
            Self::Other => 0x01,
            Self::Desktop => 0x03,
            Self::LowProfileDesktop => 0x04,
            Self::Tower => 0x07,
            Self::RackMount => 0x17,
            Self::Blade => 0x1C,
            Self::Embedded => 0x1D,
        }
    }
}

/// Processor family for SMBIOS Type 4 "Processor Family 2" field.
///
/// Maps to the SMBIOS specification processor family identifiers.
/// Used by codegen to emit the correct value for each platform.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProcessorFamily {
    /// Unknown processor family (0x02).
    #[default]
    Unknown,
    /// ARM (0x0118).
    Arm,
    /// AArch64 / ARMv8+ (0x0119).
    Aarch64,
    /// x86-64 / AMD64 / Intel 64 (0x28).
    X86_64,
    /// RISC-V (0x0135).
    RiscV,
}

impl ProcessorFamily {
    /// Convert to the SMBIOS Type 4 "Processor Family 2" 16-bit value.
    pub fn to_smbios_u16(self) -> u16 {
        match self {
            Self::Unknown => 0x02,
            Self::Arm => 0x0118,
            Self::Aarch64 => 0x0119,
            Self::X86_64 => 0x28,
            Self::RiscV => 0x0135,
        }
    }
}

/// Processor description for SMBIOS Type 4.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmbiosProcessor {
    /// Socket designation string (e.g., "CPU0").
    pub socket: HString<32>,
    /// Processor manufacturer (e.g., "ARM").
    pub manufacturer: HString<64>,
    /// Processor family for the SMBIOS "Processor Family 2" field.
    ///
    /// Defaults to `Unknown` if omitted.
    #[serde(default)]
    pub processor_family: ProcessorFamily,
    /// Maximum speed in MHz.
    pub max_speed_mhz: u16,
    /// Number of physical cores.
    pub core_count: u16,
    /// Number of threads (logical processors).
    pub thread_count: u16,
    /// Cache hierarchy for this processor (Type 7 entries).
    ///
    /// Each entry produces an SMBIOS Type 7 (Cache Information) structure.
    /// The first three entries are linked as L1/L2/L3 cache handles
    /// in the Type 4 processor entry.
    #[serde(default)]
    pub caches: heapless::Vec<SmbiosCache, 6>,
}

/// Cache description for SMBIOS Type 7.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmbiosCache {
    /// Cache socket designation string (e.g., "L1 Data Cache").
    pub designation: HString<32>,
    /// Cache level (1, 2, or 3).
    pub level: u8,
    /// Cache size in KiB.
    pub size_kb: u32,
    /// Cache associativity.
    #[serde(default)]
    pub associativity: CacheAssociativity,
    /// Cache type (data, instruction, or unified).
    #[serde(default)]
    pub cache_type: CacheType,
}

/// Cache associativity for SMBIOS Type 7.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CacheAssociativity {
    /// Unknown associativity.
    #[default]
    Unknown,
    /// Direct mapped.
    DirectMapped,
    /// 2-way set-associative.
    Way2,
    /// 4-way set-associative.
    Way4,
    /// 8-way set-associative.
    Way8,
    /// 16-way set-associative.
    Way16,
    /// Fully associative.
    FullyAssociative,
}

impl CacheAssociativity {
    /// Convert to the SMBIOS Type 7 associativity byte value.
    pub fn to_smbios_byte(self) -> u8 {
        match self {
            Self::Unknown => 0x02,
            Self::DirectMapped => 0x03,
            Self::Way2 => 0x04,
            Self::Way4 => 0x05,
            Self::Way8 => 0x07,
            Self::Way16 => 0x09,
            Self::FullyAssociative => 0x06,
        }
    }
}

/// Cache type for SMBIOS Type 7 "System Cache Type" field.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CacheType {
    /// Unified cache.
    #[default]
    Unified,
    /// Instruction cache.
    Instruction,
    /// Data cache.
    Data,
}

impl CacheType {
    /// Convert to the SMBIOS Type 7 system cache type byte value.
    pub fn to_smbios_byte(self) -> u8 {
        match self {
            Self::Unified => 0x05,
            Self::Instruction => 0x03,
            Self::Data => 0x04,
        }
    }
}

/// Memory device description for SMBIOS Type 17.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmbiosMemoryDevice {
    /// Device locator string (e.g., "DIMM0", "Bank 0").
    pub locator: HString<32>,
    /// Memory size in megabytes.
    pub size_mb: u32,
    /// Memory speed in MHz (e.g., 2400, 3200).
    pub speed_mhz: u16,
    /// Memory type.
    #[serde(default)]
    pub memory_type: MemoryDeviceType,
}

/// Memory device type (SMBIOS Type 17 field 0x12).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryDeviceType {
    /// Unknown memory type.
    #[default]
    Unknown,
    /// DDR3 SDRAM.
    Ddr3,
    /// DDR4 SDRAM.
    Ddr4,
    /// DDR5 SDRAM.
    Ddr5,
    /// LPDDR4.
    Lpddr4,
    /// LPDDR5.
    Lpddr5,
}

impl MemoryDeviceType {
    /// Convert to the SMBIOS Type 17 memory type byte value.
    pub fn to_smbios_byte(self) -> u8 {
        match self {
            Self::Unknown => 0x02,
            Self::Ddr3 => 0x18,
            Self::Ddr4 => 0x1A,
            Self::Ddr5 => 0x22,
            Self::Lpddr4 => 0x1B,
            Self::Lpddr5 => 0x23,
        }
    }
}

fn default_bios_date() -> HString<16> {
    HString::try_from("01/01/2026").unwrap_or_default()
}
