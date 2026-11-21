//! Firmware Filesystem: read, verify, and build.
//!
//! ## Reader (no_std, no alloc)
//!
//! The reader operates on a `&[u8]` slice representing the firmware image
//! (typically the memory-mapped flash region). It can:
//!
//! - Locate the `LocatorBlock` (at a known offset or by scanning for `LOCATOR_MAGIC`)
//! - Authenticate the signed 512-byte boot root against constant trust policy
//! - Parse the revision-2 directory (`DirectoryView`) referenced by the root
//! - Look up files by name or type and read segment data from the image
//!
//! ## Builder (std, fbuild)
//!
//! The builder module (behind `std` feature) constructs FFS images:
//! assemble files + segments, compute digests, encode the directory and
//! signed boot root, and produce the final binary.

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(feature = "std")]
extern crate alloc;

#[cfg(feature = "directory")]
pub mod directory;
pub mod lz4;
#[cfg(feature = "directory")]
pub mod reader;
pub mod root;

#[cfg(feature = "std")]
pub mod builder;

#[cfg(feature = "directory")]
pub use directory::{DirectorySummary, DirectoryView, FileView};
#[cfg(feature = "directory")]
pub use reader::{FfsReader, ReaderError};
