//! Experimental IBASE MB899: i945GM/ICH7-M and W83627EHG policy.
//! Protected-mode stages support Socket M Core Duo and Core 2 configurations.
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
