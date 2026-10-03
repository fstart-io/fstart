// SPDX-License-Identifier: MIT OR Apache-2.0
//! AMD family flows. Picasso starts in PSP-trained DRAM, never Intel CAR.
#![no_std]
#[cfg(feature = "picasso")]
pub mod early;
#[cfg(feature = "picasso")]
pub mod memory;
#[cfg(all(test, feature = "picasso"))]
mod memory_tests;
#[cfg(feature = "picasso")]
pub mod picasso;
