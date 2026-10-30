//! Foxconn D41S board support package.

#![no_std]

extern crate alloc;

pub mod config;
pub mod mainboard;
#[cfg(feature = "smm")]
pub mod smm;

/// Foxconn D41S board marker selected by the board-owned stage entry.
pub struct Board;

pub use config::*;
