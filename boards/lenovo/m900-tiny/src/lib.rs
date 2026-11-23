// SPDX-License-Identifier: MIT OR Apache-2.0
//! Lenovo ThinkCentre M700/M900 Tiny: Kaby Lake (or Skylake) desktop with a
//! Sunrise Point-H PCH and an NCT6687D Super I/O (experimental).
#![no_std]

#[cfg(feature = "acpi")]
extern crate alloc;

#[cfg(feature = "acpi")]
pub mod acpi;
pub mod gpio;
pub mod hda;
pub mod hwm;
pub mod mainboard;
pub mod pch;
pub mod pcie;
pub mod profile;
pub mod sata;
#[cfg(feature = "smm")]
mod smm;
pub mod superio;
pub mod usb;

pub struct Board;
