//! Core no_std types, MMIO/PIO primitives, and service traits for fstart.
//!
//! These types define the Rust board configuration schema, firmware filesystem
//! structures, and security primitives.
//!
//! `no_std` by default — uses `heapless` collections for bounded containers.

#![cfg_attr(not(feature = "std"), no_std)]

pub mod acpi;
pub mod board;
pub mod builder;
pub mod const_vec;
pub mod device;
pub mod ffs;
pub mod handoff;
pub mod memory;
pub mod security;
pub mod smbios;
pub mod smm;
pub mod stage;
pub mod typed;

pub mod mmio;
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub mod pio;
pub mod services;

pub use acpi::{
    AcpiAhciDevice, AcpiConfig, AcpiGenericDevice, AcpiPcieRootDevice, AcpiPlatform, AcpiResource,
    AcpiWatchdog, AcpiXhciDevice, ArmPlatformAcpi,
};
pub use board::{
    BoardBuildPolicy, BoardConfig, FdtSource, FirmwareConfig, FirmwareImagePolicy, FirmwareKind,
    FitParseMode, PayloadConfig, PayloadKind, Platform, QemuMachine, SocImageFormat,
};
pub use builder::{dev_security_config, hstr, hvec, x86_linuxboot_payload, x86_uefi_payload};
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
pub use security::{DigestAlgorithm, SecurityConfig, SignatureAlgorithm};
pub use smbios::SmbiosConfig;
pub use smm::{CorebootSmmCompat, SmmConfig};
pub use stage::{
    FirmwareImageConfig, MonolithicConfig, MpBuildConfig, POSTCAR_STAGE_NAME, RunsFrom,
    StageBuildConfig, StageConfig, StageLayout, TempRamBuffer, effective_stage_load_addr,
};
pub use typed::{Io8, Io16, IoAddr, Irq, Mmio8, Mmio16, Mmio32, Mmio64, MmioAddr, io16, mmio32};
