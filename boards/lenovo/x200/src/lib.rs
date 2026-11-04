//! Lenovo ThinkPad X200 board support package (8-MiB 6D flash layout,
//! integrated GM45 graphics).
//!
//! The boot flow is the board's: one `main` per stage in `src/bin/`. This
//! library holds the board's facts and configuration and the mainboard work
//! those `main`s call. It has no stage cfgs; each stage links what its
//! `main` reaches.

#![no_std]

#[cfg(test)]
extern crate std;

pub mod acpi;
pub mod config;
pub mod ec;
pub mod mainboard;
/// The SMM handler, built into the separate SMM image.
#[cfg(feature = "smm")]
pub mod smm;

/// Lenovo ThinkPad X200 board marker: its facts are [`config`].
pub struct Board;

pub use config::*;
