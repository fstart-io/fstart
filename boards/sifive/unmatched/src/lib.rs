//! HiFive Unmatched board support package.

#![no_std]

extern crate ufmt;

pub mod config;
#[cfg(fstart_stage_env = "monolithic")]
pub mod fu740;
#[cfg(fstart_stage_env = "monolithic")]
mod stage;

/// HiFive Unmatched board marker selected by the board-owned stage entry.
pub struct Board;

pub use config::*;
