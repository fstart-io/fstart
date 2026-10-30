//! Intel D945GCLF board support package.
//!
//! The boot flow is the board's: one `main` per stage in `src/bin/`. This
//! library holds the board's facts and configuration and the mainboard work
//! those `main`s call.

#![no_std]

extern crate alloc;

pub mod config;
pub mod mainboard;
/// The SMM handler, built into the separate SMM image.
#[cfg(feature = "smm")]
pub mod smm;

/// Intel D945GCLF board marker: its facts are [`config`].
pub struct Board;

pub use config::*;
