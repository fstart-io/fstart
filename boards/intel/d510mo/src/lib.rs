//! Intel D510MO Pineview/NM10 board support.
#![no_std]

#[cfg(feature = "acpi")]
extern crate alloc;

pub mod config;
pub mod mainboard;
#[cfg(feature = "smm")]
pub mod smm;

pub struct Board;
pub use config::*;
