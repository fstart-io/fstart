//! HiFive Unmatched board support package.

#![no_std]

extern crate ufmt;

pub mod config;
/// The FU740 flow the stage `main` runs: firmware only.
#[cfg(all(target_arch = "riscv64", target_os = "none"))]
pub mod fu740;

/// HiFive Unmatched board marker.
pub struct Board;

pub use config::*;
