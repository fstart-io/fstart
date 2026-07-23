//! QEMU SiFive U board support package.

#![no_std]

pub mod config;
#[cfg(feature = "stage")]
mod stage;

/// QEMU SiFive U board marker selected by the board-owned stage entry.
pub struct Board;

pub use config::*;
