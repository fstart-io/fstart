//! HiFive Unmatched board support package.

#![no_std]

#[cfg(fstart_stage_env = "ram")]
extern crate alloc;
extern crate ufmt;

pub mod config;
#[cfg(any(fstart_stage_env = "car", fstart_stage_env = "ram"))]
pub mod fu740;
#[cfg(any(fstart_stage_env = "car", fstart_stage_env = "ram"))]
mod stage;

/// HiFive Unmatched board marker selected by the board-owned stage entry.
pub struct Board;

pub use config::*;
