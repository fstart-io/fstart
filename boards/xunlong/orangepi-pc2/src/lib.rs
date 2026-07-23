//! Xunlong Orange Pi PC2 board support package.

#![no_std]

pub mod config;
#[cfg(feature = "stage")]
mod stage;

/// Orange Pi PC2 marker selected by the board-owned stage entry.
pub struct Board;

pub use config::*;
