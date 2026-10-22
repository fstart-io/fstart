// SPDX-License-Identifier: MIT OR Apache-2.0
//! AMD hardware mechanisms. No board discovery, stage loading or FSP calls.
#![no_std]
#[cfg(feature = "picasso")]
pub mod fch;
#[cfg(feature = "picasso")]
pub mod gpio;
#[cfg(all(test, feature = "picasso"))]
mod gpio_tests;
#[cfg(feature = "picasso")]
pub mod picasso;
