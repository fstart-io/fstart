// SPDX-License-Identifier: MIT OR Apache-2.0
//! AMD family flows. Picasso starts in PSP-trained DRAM, never Intel CAR.
#![no_std]
#[cfg(feature = "picasso")]
pub mod picasso;
