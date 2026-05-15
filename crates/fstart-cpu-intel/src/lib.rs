//! Intel CPU support for fstart.
//!
//! Groups per-family CPU initialization and ACPI power-management helpers for
//! Intel x86 platforms.

#![no_std]

pub mod pineview;

#[cfg(feature = "acpi")]
pub mod core2;
