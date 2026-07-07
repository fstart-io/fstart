//! Foxconn D41S board support package.

#![no_std]

pub mod config;
pub mod mainboard;
#[cfg(feature = "stage")]
mod stage;

/// Foxconn D41S board marker selected by the board-owned stage entry.
pub struct Board;

pub use config::*;
#[cfg(feature = "acpi")]
pub use mainboard::d41s_mainboard_dsdt_aml;
#[cfg(feature = "stage")]
pub use mainboard::D41SMainboard;
