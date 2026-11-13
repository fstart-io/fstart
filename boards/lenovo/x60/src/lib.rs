//! Lenovo ThinkPad X60 board support package.

#![no_std]

#[cfg(test)]
extern crate std;

#[cfg(feature = "acpi")]
pub mod acpi;
pub mod config;
pub mod mainboard;
#[cfg(feature = "smm")]
pub mod smm;
pub mod x6;

/// Lenovo ThinkPad X60 board marker.
pub struct Board;

pub use config::*;
