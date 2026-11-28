//! Intel CPU support for fstart.
//!
//! Groups per-family CPU initialization, SMM relocation and ACPI
//! power-management helpers for Intel x86 platforms.

mod common_power;
pub mod core2_cpu;
pub mod feature_control;
pub mod haswell;
mod haswell_vr;
pub mod microcode;
pub mod model_2065x;
pub mod model_206ax;
pub mod penryn;
pub mod pineview;
pub mod ratio;
pub mod silvermont;
pub mod skylake;
pub mod smm;
pub mod smrr;
pub mod speedstep;
