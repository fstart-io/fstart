// SPDX-License-Identifier: MIT OR Apache-2.0
//! Compatibility facade for the existing image builders over the shared
//! format reader.
#![no_std]
pub use fstart_amdfw_format::{Error, Flash, Profile};
pub mod directory;
pub mod efs;
#[cfg(test)]
mod tests;
