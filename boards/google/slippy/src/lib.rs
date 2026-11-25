// SPDX-License-Identifier: MIT OR Apache-2.0
//! Acer C720/C720P (Google Peppy), a Slippy-family Chromebook.
//!
//! [`slippy`] holds the family wiring and EC handoff that Falco, Leon and
//! Wolf share; pads, SPD, codec and device population here are Peppy's.
#![no_std]
#[cfg(test)]
extern crate std;
#[cfg(feature = "acpi")]
pub mod acpi;
mod config;
mod hda;
pub mod options;
mod pads;
pub mod slippy;
#[cfg(feature = "smm")]
pub mod smm;
mod spd;
#[cfg(test)]
mod tests;

pub use config::PLATFORM;
pub struct Board;
