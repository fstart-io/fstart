//! Core no_std types, MMIO/PIO primitives, and service traits for fstart.
//!
//! These types define the Rust board configuration schema, firmware filesystem
//! structures, and security primitives.
//!
//! `no_std` by default — uses `heapless` collections for bounded containers.

#![cfg_attr(not(feature = "std"), no_std)]

pub mod acpi;
#[cfg(feature = "host")]
pub mod board;
#[cfg(feature = "host")]
pub mod builder;
pub mod checksum;
pub mod const_vec;
pub mod description;
pub mod device;
pub mod ffs;
pub mod handoff;
pub mod indexed;
pub mod layout;
pub mod memory;
pub mod memory_info;
pub mod meta;
pub mod once;
pub mod platform;
pub mod security;
pub mod smbios;
pub mod smm;
pub mod stage;
pub mod typed;

pub mod mmio;
pub mod payload_manifest;
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub mod pio;
pub mod services;

pub use acpi::{
    AcpiAhciDevice, AcpiConfig, AcpiGenericDevice, AcpiInterruptFlags, AcpiPcieRootDevice,
    AcpiPlatform, AcpiResource, AcpiWatchdog, AcpiXhciDevice, ArmPlatformAcpi,
};
#[cfg(feature = "host")]
pub use board::{
    BoardBuildPolicy, BoardConfig, FdtSource, FirmwareConfig, FirmwareImagePolicy, FitParseMode,
    PayloadConfig, PayloadKind, SocImageFormat,
};
#[cfg(feature = "host")]
pub use builder::{
    default_security_config, elf_payload, x86_coreboot_payload, x86_linux_payload, x86_uefi_payload,
};
pub use const_vec::ConstVec;
pub use device::BusAddress;
pub use ffs::{
    AnchorBlock, Compression, DigestSet, EntryContent, FFS_MAGIC, FFS_VERSION, FileType,
    ImageManifest, KeyBytes, Region, RegionContent, RegionEntry, Segment, SegmentFlags,
    SegmentKind, Signature, SignatureKind, VerificationKey,
};
pub use memory::{
    CarConfig, FlashLayout, IntelIfdFlashLayout, IntelIfdRegion, IntelIfdRegionConfig, MemoryMap,
    MemoryMapError, MemoryRegion, RegionKind, X86LegacyFlashLayout,
};
pub use meta::{hstr, hvec};
pub use platform::{FirmwareKind, Platform, QemuMachine};
pub use security::{DigestAlgorithm, SecurityConfig, SignatureAlgorithm};
pub use smbios::SmbiosConfig;
pub use smm::{CorebootSmmCompat, SmmConfig};
#[cfg(feature = "host")]
pub use stage::{
    FirmwareImageConfig, MonolithicConfig, MpBuildConfig, RunsFrom, StageBuildConfig, StageConfig,
    StageLayout, effective_stage_load_addr,
};
pub use stage::{POSTCAR_STAGE_NAME, TempRamBuffer};
pub use typed::{Io8, Io16, IoAddr, Irq, Mmio8, Mmio16, Mmio32, Mmio64, MmioAddr, io16, mmio32};
