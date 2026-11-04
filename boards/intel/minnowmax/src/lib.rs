//! MinnowBoard MAX: Bay Trail E38xx with memory-down DDR3L.
#![no_std]

#[cfg(feature = "acpi")]
extern crate alloc;

pub mod config;
pub mod gpio;
pub mod mainboard;
#[cfg(feature = "smm")]
pub mod smm;
pub mod spd;

pub struct Board;
pub use config::*;
