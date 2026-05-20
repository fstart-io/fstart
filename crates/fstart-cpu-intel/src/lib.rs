//! Intel CPU support for fstart.
//!
//! Groups per-family CPU initialization and ACPI power-management helpers for
//! Intel x86 platforms.

#![no_std]

pub mod core2_cpu;
pub mod pineview;
pub mod sandybridge;

#[cfg(feature = "acpi")]
pub mod core2;
