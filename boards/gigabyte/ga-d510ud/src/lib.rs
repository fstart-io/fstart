//! Gigabyte GA-D510UD Pineview/NM10 board support.
#![no_std]

extern crate alloc;

pub mod config;
pub mod mainboard;
#[cfg(feature = "smm")]
pub mod smm;

pub struct Board;
pub use config::*;
