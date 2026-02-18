//! Firmware Filesystem: read, verify, and build.
//!
//! ## Reader (no_std, no alloc)
//!
//! The reader operates on a `&[u8]` slice representing the firmware image
//! (typically the memory-mapped flash region). It can:
//!
//! - Find the anchor block (by scanning for `FFS_MAGIC` or at a known offset)
//! - Deserialize and verify the RO manifest
//! - Follow pointers to RW manifests and verify those
//! - Look up files by name
//! - Read segment data from the image
//!
//! ## Builder (std, xtask)
//!
//! The builder module (behind `std` feature) constructs FFS images:
//! assemble files + segments, compute digests, build manifests, sign,
//! and produce the final binary.

#![cfg_attr(not(feature = "std"), no_std)]

pub mod lz4;
pub mod reader;

#[cfg(feature = "std")]
pub mod builder;

pub use reader::{verify_and_parse_manifest, FfsReader, ReaderError};
