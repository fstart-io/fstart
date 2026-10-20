// SPDX-License-Identifier: MIT OR Apache-2.0
//! The shared flow of the PSP-booted ("noncar") AMD SoCs from Family 17h on:
//! the PSP trains DRAM
//! and starts the BSP in DRAM, the firmware runs from the reset vector to the
//! payload in one stage.
#[cfg(all(feature = "bundle-zeppelin", target_os = "none"))]
pub mod acpi;
#[cfg(feature = "host")]
pub mod host;
pub mod layout;
pub mod memory;
#[cfg(all(feature = "bundle-zeppelin", target_os = "none"))]
pub mod boot;
#[cfg(all(feature = "bundle-zeppelin", target_os = "none"))]
pub mod pci;
#[cfg(all(feature = "bundle-zeppelin", target_os = "none"))]
pub mod psp;
#[cfg(all(feature = "bundle-zeppelin", target_os = "none"))]
pub mod smm;

/// The most dies the flow handles: two sockets of four.
pub const MAX_DIES: usize = 8;
