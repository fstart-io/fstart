//! Lenovo ThinkPad T400 / T500 / R400 / W500 / R500 board support package
//! (GM45/ICH9M, Socket P, optional hybrid/discrete graphics, Mini Dock serial).
//!
//! The boot flow is the board's: one `main` per stage in `src/bin/`. This
//! library holds the board's facts and configuration and the mainboard work
//! those `main`s call. Each stage links what its `main` reaches; only the
//! ACPI tables, which need the ramstage heap, sit behind a feature.

#![no_std]

#[cfg(test)]
extern crate std;

#[cfg(feature = "acpi")]
pub mod acpi;
pub mod config;
pub mod ec;
pub mod mainboard;
/// The SMM handler, built into the separate SMM image.
#[cfg(feature = "smm")]
pub mod smm;

/// Lenovo ThinkPad T400-family board marker: its facts are [`config`].
pub struct Board;

pub use config::*;
