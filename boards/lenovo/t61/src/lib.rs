//! Lenovo ThinkPad T61/T61p board support (experimental, not hardware validated).
#![no_std]

#[cfg(test)]
extern crate std;

#[cfg(feature = "acpi")]
pub mod acpi;
pub mod config;
pub mod mainboard;
#[cfg(feature = "smm")]
pub mod smm;

pub struct Board;
pub use config::*;
#[cfg(all(test, feature = "acpi"))]
mod tests;
