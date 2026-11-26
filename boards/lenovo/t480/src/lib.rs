// SPDX-License-Identifier: MIT OR Apache-2.0
//! Lenovo ThinkPad T480: Kaby Lake-U / Kaby Lake-R mobile platform with a
//! Sunrise Point-LP PCH, Microchip MEC1653 / Lenovo H8 EC, and PMH7 hub.
#![no_std]

#[cfg(feature = "acpi")]
extern crate alloc;
#[cfg(test)]
extern crate std;

#[cfg(feature = "acpi")]
pub mod acpi;
pub mod gpio;
pub mod hda;
pub mod mainboard;
pub mod pch;
pub mod pcie;
pub mod profile;
pub mod sata;
#[cfg(feature = "smm")]
mod smm;
pub mod usb;

pub struct Board;
