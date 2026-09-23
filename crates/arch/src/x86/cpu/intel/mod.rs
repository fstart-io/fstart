//! Intel CPU support for fstart.
//!
//! Groups per-family CPU initialization, SMM relocation and ACPI
//! power-management helpers for Intel x86 platforms.

mod common_power;
pub mod core2_cpu;
pub mod feature_control;
pub mod microcode;
mod msr_register;
pub mod pineview;
pub mod smm;
pub mod smrr;
