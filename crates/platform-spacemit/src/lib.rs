//! Fixed SpacemiT platform flows.
//!
//! K3: the boot ROM loads the first stage into on-chip SRAM from a block
//! device or over USB fastboot and enters it in M-mode on hart 0.

#![no_std]

pub mod facts;
#[cfg(feature = "host")]
pub mod host;

#[cfg(feature = "host")]
pub use host::Plan;
