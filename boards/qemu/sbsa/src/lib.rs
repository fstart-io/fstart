//! QEMU SBSA-reference board package.
#![no_std]

pub mod config;
#[cfg(feature = "stage")]
mod stage;

pub struct Board;
pub use config::*;
