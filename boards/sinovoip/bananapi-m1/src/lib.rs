//! LeMaker Banana Pi M1 board support package.

#![no_std]

pub mod config;
#[cfg(any(fstart_stage_env = "car", fstart_stage_env = "ram"))]
mod stage;

/// Banana Pi M1 marker selected by the board-owned stage entry.
pub struct Board;

pub use config::*;
