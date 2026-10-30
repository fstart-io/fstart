//! ASUS P5GC-MX i945GC/ICH7 and desktop model-6FX support.
#![no_std]

extern crate alloc;

pub mod config;
pub mod mainboard;
#[cfg(feature = "smm")]
pub mod smm;

pub struct Board;
pub use config::*;
