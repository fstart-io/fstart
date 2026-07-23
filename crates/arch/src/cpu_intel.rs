//! Intel CPU support for fstart.
//!
//! Groups per-family CPU initialization and ACPI power-management helpers for
//! Intel x86 platforms.

pub mod core2_cpu;
pub mod microcode;
pub mod pineview;

#[cfg(feature = "acpi")]
pub mod core2;
