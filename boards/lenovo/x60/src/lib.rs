//! Lenovo ThinkPad X60 board support package.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod config;
pub mod mainboard;
#[cfg(feature = "smm")]
pub mod smm;

/// Lenovo ThinkPad X60 board marker.
pub struct Board;

pub use config::*;
