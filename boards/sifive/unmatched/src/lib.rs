//! HiFive Unmatched board support package.

#![no_std]

#[cfg(feature = "stage")]
extern crate ufmt;

pub mod config;
#[cfg(feature = "stage")]
mod fu740;
#[cfg(feature = "stage")]
mod stage;

/// HiFive Unmatched board marker selected by the board-owned stage entry.
pub struct Board;

pub use config::*;
