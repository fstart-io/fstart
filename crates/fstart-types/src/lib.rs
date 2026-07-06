//! Core types for the fstart firmware framework.
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

pub use acpi::{
    AcpiAhciDevice, AcpiConfig, AcpiGenericDevice, AcpiPcieRootDevice, AcpiPlatform, AcpiResource,
    AcpiWatchdog, AcpiXhciDevice, ArmPlatformAcpi,
};
pub use board::{
    BoardBuildPolicy, BoardConfig, FdtSource, FirmwareConfig, FirmwareImagePolicy, FirmwareKind,
    FitParseMode, PayloadConfig, PayloadKind, Platform, SocImageFormat,
};
pub use builder::{dev_security_config, hstr, hvec, x86_linuxboot_payload, x86_uefi_payload};
pub use const_vec::ConstVec;
pub use device::{BusAddress, DeviceConfig, DeviceId, DeviceNode, DeviceRole};
pub use ffs::{
    AnchorBlock, Compression, DigestSet, EntryContent, FileType, ImageManifest, KeyBytes, Region,
    RegionContent, RegionEntry, Segment, SegmentFlags, SegmentKind, Signature, SignatureKind,
    VerificationKey, FFS_MAGIC, FFS_VERSION,
};
pub use memory::{
    CarConfig, FlashLayout, IntelIfdFlashLayout, IntelIfdRegion, IntelIfdRegionConfig, MemoryMap,
    MemoryMapError, MemoryRegion, RegionKind,
};
pub use security::{DigestAlgorithm, SecurityConfig, SignatureAlgorithm};
pub use smbios::SmbiosConfig;
pub use smm::{CorebootSmmCompat, SmmConfig};
pub use stage::{
    effective_stage_load_addr, BootMedium, Capability, MonolithicConfig, RunsFrom, StageConfig,
    StageLayout, TempRamBuffer,
};
pub use typed::{
    io16, mmio32, Io16, Io8, IoAddr, Irq, Mmio16, Mmio32, Mmio64, Mmio8, MmioAddr, PciBdf,
};
