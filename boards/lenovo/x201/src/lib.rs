//! Lenovo ThinkPad X201 board support package (Arrandale with the
//! Ironlake uncore, QM57 PCH, 8-MiB flash).
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
pub mod mainboard;
/// The SMM handler, built into the separate SMM image.
#[cfg(feature = "smm")]
pub mod smm;

/// Lenovo ThinkPad X201 board marker: its facts are [`config`].
pub struct Board;

pub use config::*;
