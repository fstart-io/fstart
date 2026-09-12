//! QEMU SBSA-reference board package.
#![no_std]

pub mod config;
#[cfg(fstart_stage_env = "monolithic")]
mod stage;

/// QEMU SBSA-reference board marker selected by the board-owned stage entry.
pub struct Board;

pub use config::*;
