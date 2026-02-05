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

pub use board::{BoardConfig, BuildMode, FdtSource, PayloadConfig, PayloadKind};
pub use device::{DeviceConfig, Resources};
pub use ffs::{
    Compression, DigestSet, FfsHeader, FileEntry, FileType, Manifest, Signature, SignedManifest,
    FFS_MAGIC,
};
pub use memory::{MemoryMap, MemoryRegion, RegionKind};
pub use security::{DigestAlgorithm, SecurityConfig, SignatureAlgorithm};
pub use stage::{Capability, MonolithicConfig, RunsFrom, StageConfig, StageLayout};
