//! Lenovo ThinkPad X61 board support package.

#![no_std]

pub mod config;
pub mod mainboard;
#[cfg(feature = "smm")]
pub mod smm;
#[cfg(feature = "stage")]
mod stage;

/// Lenovo ThinkPad X61 board marker for selected-board stage wrappers.
pub struct Board;

pub use config::*;
#[cfg(feature = "acpi")]
pub use mainboard::x61_mainboard_dsdt_aml;
pub use mainboard::{LenovoX61Southbridge, X61_SMBIOS_DESC};
