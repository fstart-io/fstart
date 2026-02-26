//! Core types for the fstart firmware framework.
//!
//! These types define the board configuration schema (RON-deserializable),
//! firmware filesystem structures, and security primitives.
//!
//! `no_std` by default — uses `heapless` collections for bounded containers.

#![cfg_attr(not(feature = "std"), no_std)]

pub mod board;
pub mod device;
pub mod ffs;
pub mod memory;
pub mod security;
pub mod stage;

pub use board::{
    BoardConfig, BuildMode, FdtSource, FirmwareConfig, FirmwareKind, PayloadConfig, PayloadKind,
};
pub use device::DeviceConfig;
pub use ffs::{
    AnchorBlock, Compression, DigestSet, EntryContent, FileType, ImageManifest, KeyBytes, Region,
    RegionContent, RegionEntry, Segment, SegmentFlags, SegmentKind, Signature, SignatureKind,
    SignedManifest, VerificationKey, FFS_MAGIC, FFS_VERSION,
};
pub use memory::{MemoryMap, MemoryRegion, RegionKind};
pub use security::{DigestAlgorithm, SecurityConfig, SignatureAlgorithm};
pub use stage::{BootMedium, Capability, MonolithicConfig, RunsFrom, StageConfig, StageLayout};
