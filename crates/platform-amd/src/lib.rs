// SPDX-License-Identifier: MIT OR Apache-2.0
//! AMD family flows. Picasso starts in PSP-trained DRAM, never Intel CAR.
#![no_std]
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
