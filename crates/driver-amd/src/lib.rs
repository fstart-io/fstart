// SPDX-License-Identifier: MIT OR Apache-2.0
//! AMD hardware mechanisms. No board discovery, stage loading or FSP calls.
#![no_std]
#[cfg(any(feature = "picasso", feature = "zeppelin"))]
pub mod apob;
#[cfg(any(feature = "picasso", feature = "zeppelin"))]
pub mod df;
#[cfg(any(feature = "picasso", feature = "zeppelin"))]
pub mod fam17h;
#[cfg(any(feature = "picasso", feature = "zeppelin"))]
pub mod fch;
#[cfg(any(feature = "picasso", feature = "zeppelin"))]
pub mod gpio;
#[cfg(all(test, feature = "picasso"))]
mod gpio_tests;
#[cfg(any(feature = "picasso", feature = "zeppelin"))]
pub mod lpc;
#[cfg(feature = "picasso")]
pub mod picasso;
#[cfg(any(feature = "picasso", feature = "zeppelin"))]
pub mod psp;
#[cfg(all(test, any(feature = "picasso", feature = "zeppelin")))]
mod silicon_tests;
#[cfg(any(feature = "picasso", feature = "zeppelin"))]
pub mod smn;
#[cfg(any(feature = "picasso", feature = "zeppelin"))]
pub mod smu;
#[cfg(any(feature = "picasso", feature = "zeppelin"))]
pub mod soc;
#[cfg(any(feature = "picasso", feature = "zeppelin"))]
pub mod table;
#[cfg(feature = "zeppelin")]
pub mod zeppelin;
