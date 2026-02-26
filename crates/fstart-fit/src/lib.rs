//! FIT (Flattened Image Tree) parser for fstart.
//!
//! Parses U-Boot FIT images, which are DTB/FDT blobs with a specific node
//! convention for bundling kernel, ramdisk, FDT, and firmware images with
//! hash integrity and configuration selection.
//!
//! ## Dual-target design
//!
//! This crate is `no_std` by default and requires no allocator. The parser
//! operates on `&[u8]` slices via zero-copy FDT parsing (dtoolkit). The
//! same parsing code runs at:
//!
//! - **Buildtime** (xtask, `std` feature): read `.itb` file from disk,
//!   extract components, embed in FFS as separate entries.
//! - **Runtime** (firmware, `no_std`): parse FIT blob from flash, extract
//!   and load components to their specified addresses.

#![cfg_attr(not(feature = "std"), no_std)]

pub mod parser;

pub use parser::{
    FitArch, FitCompression, FitConfig, FitError, FitHash, FitImage, FitImageNode, FitImageType,
};
