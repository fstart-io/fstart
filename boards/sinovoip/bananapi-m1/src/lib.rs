//! LeMaker Banana Pi M1 board support package.

#![no_std]

pub mod config;
#[cfg(feature = "stage")]
mod stage;

/// Banana Pi M1 marker selected by the board-owned stage entry.
pub struct Board;

pub use config::*;
