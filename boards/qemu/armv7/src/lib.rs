//! QEMU virt board support package.

#![no_std]

pub mod config;
#[cfg(fstart_stage_env = "monolithic")]
mod stage;

/// QEMU virt board marker selected by the board-owned stage entry.
pub struct Board;

pub use config::*;
