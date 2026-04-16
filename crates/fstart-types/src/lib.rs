//! Core types for the fstart firmware framework.
//!
//! These types define the board configuration schema (RON-deserializable),
//! firmware filesystem structures, and security primitives.
//!
//! `no_std` by default — uses `heapless` collections for bounded containers.

#![cfg_attr(not(feature = "std"), no_std)]

pub mod acpi;
pub mod board;
pub mod device;
pub mod ffs;
pub mod handoff;
pub mod memory;
pub mod security;
pub mod smbios;
pub mod stage;

pub use acpi::{
    AcpiAhciDevice, AcpiConfig, AcpiGenericDevice, AcpiPcieRootDevice, AcpiPlatform, AcpiResource,
    AcpiWatchdog, AcpiXhciDevice, ArmPlatformAcpi,
};
pub use board::{
    BoardConfig, BuildMode, FdtSource, FirmwareConfig, FirmwareKind, FitParseMode, PayloadConfig,
    PayloadKind, Platform, SocImageFormat,
};
pub use device::{BusAddress, DeviceConfig, DeviceId, DeviceNode};
pub use ffs::{
    AnchorBlock, Compression, DigestSet, EntryContent, FileType, ImageManifest, KeyBytes, Region,
    RegionContent, RegionEntry, Segment, SegmentFlags, SegmentKind, Signature, SignatureKind,
    SignedManifest, VerificationKey, FFS_MAGIC, FFS_VERSION,
};
pub use memory::{CarConfig, CarMethod, MemoryMap, MemoryRegion, RegionKind};
pub use security::{DigestAlgorithm, SecurityConfig, SignatureAlgorithm};
pub use smbios::SmbiosConfig;
pub use stage::{
    AutoBootDevice, BootMedium, Capability, LoadDevice, MonolithicConfig, RunsFrom, StageConfig,
    StageLayout,
};
