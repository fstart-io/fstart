//! Intel CPU support for fstart.
//!
//! Groups per-family CPU initialization, SMM relocation and ACPI
//! power-management helpers for Intel x86 platforms.

pub mod core2_cpu;
pub mod microcode;
pub mod pineview;
pub mod smm;
