//! Lenovo ThinkPad X61 board support package.

#![no_std]

pub mod config;
pub mod mainboard;
#[cfg(feature = "smm")]
pub mod smm;
#[cfg(feature = "stage")]
mod stage;

/// Lenovo ThinkPad X61 board marker selected by the board-owned stage entry.
pub struct Board;

pub use config::*;
#[cfg(feature = "acpi")]
pub use mainboard::x61_mainboard_dsdt_aml;
#[cfg(feature = "stage")]
pub use mainboard::X61Mainboard;
#[cfg(feature = "smbios")]
pub use mainboard::X61_SMBIOS_DESC;
