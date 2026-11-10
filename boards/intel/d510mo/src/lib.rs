//! Intel D510MO Pineview/NM10 board support.
#![no_std]

#[cfg(feature = "acpi")]
pub mod acpi;
pub mod config;
pub mod mainboard;
#[cfg(feature = "smm")]
pub mod smm;

pub struct Board;
pub use config::*;
