//! QEMU q35 board support package for the 32-bit (i686) build.

#![no_std]

pub mod config;
#[cfg(fstart_stage_env = "monolithic")]
mod stage;
#[cfg(fstart_stage_env = "smm")]
pub mod smm;

/// QEMU q35 board marker selected by the board-owned stage entry.
pub struct Board;

pub use config::*;
