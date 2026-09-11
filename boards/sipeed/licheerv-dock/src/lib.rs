//! Xunlong Lichee RV Dock board support package.

#![no_std]

pub mod config;
#[cfg(any(fstart_stage_env = "car", fstart_stage_env = "ram"))]
mod stage;

/// Lichee RV Dock marker selected by the board-owned stage entry.
pub struct Board;

pub use config::*;
