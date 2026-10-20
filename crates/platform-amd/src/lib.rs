// SPDX-License-Identifier: MIT OR Apache-2.0
//! AMD family flows. Picasso and Zeppelin start in PSP-trained DRAM, never
//! Intel CAR.
#![no_std]
#[cfg(all(feature = "host", feature = "picasso"))]
pub mod host;
// Each board's host adapter enables exactly one SoC.
#[cfg(all(feature = "host", feature = "picasso", not(feature = "zeppelin")))]
pub use host::Plan;
#[cfg(all(feature = "host", feature = "zeppelin"))]
pub use zeppelin::host::Plan;
#[cfg(feature = "zeppelin")]
pub mod zeppelin;
#[cfg(feature = "bundle-zeppelin")]
#[doc(hidden)]
pub use fstart_stage as __stage;
#[cfg(all(feature = "picasso", target_arch = "x86_64"))]
pub mod cold;
#[cfg(all(feature = "picasso", target_arch = "x86_64"))]
pub mod cpu;
#[cfg(feature = "picasso")]
pub mod early;
#[cfg(feature = "picasso")]
pub mod layout;
#[cfg(feature = "picasso")]
pub mod memory;
#[cfg(all(test, feature = "picasso"))]
mod memory_tests;
#[cfg(all(feature = "picasso", target_arch = "x86_64"))]
pub mod microcode;
#[cfg(feature = "picasso")]
pub mod mp;
#[cfg(feature = "picasso")]
pub mod picasso;
#[cfg(all(feature = "picasso", target_arch = "x86_64"))]
pub mod psp;
