//! QEMU SBSA-reference board package.
#![no_std]

pub mod config;

/// QEMU SBSA-reference board marker.
pub struct Board;

pub use config::*;
