//! Foxconn D41S board support package.

#![no_std]

#[cfg(feature = "acpi")]
pub mod acpi;
pub mod config;
pub mod mainboard;
#[cfg(feature = "smm")]
pub mod smm;

/// Foxconn D41S board marker selected by the board-owned stage entry.
pub struct Board;

pub use config::*;
