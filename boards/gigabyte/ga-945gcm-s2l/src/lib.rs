//! Shared Gigabyte GA-945GCM-S2L/S2C wiring and board sequencing.
#![no_std]

extern crate alloc;

pub mod config;
pub mod mainboard;
#[cfg(feature = "smm")]
pub mod smm;

pub struct Board;
pub use config::*;
