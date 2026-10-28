// SPDX-License-Identifier: MIT OR Apache-2.0
//! AMD hardware mechanisms. No board discovery, stage loading or FSP calls.
//!
//! The modules are IP blocks, each with its common code and a submodule
//! per IP version where the code differs; [`soc`] says which versions a SoC
//! is made of. The register transports are [`smn`] and [`df`].
#![cfg_attr(not(test), no_std)]
#[cfg(feature = "picasso")]
pub mod acp;
#[cfg(any(feature = "picasso", feature = "zeppelin"))]
pub mod apob;
#[cfg(any(feature = "picasso", feature = "zeppelin"))]
pub mod ccx;
#[cfg(any(feature = "picasso", feature = "zeppelin"))]
pub mod df;
#[cfg(any(feature = "picasso", feature = "zeppelin"))]
pub mod dxio;
#[cfg(any(feature = "picasso", feature = "zeppelin"))]
pub mod fch;
#[cfg(any(feature = "picasso", feature = "zeppelin"))]
pub mod nbio;
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
/// The SP3 server silicon: Naples, Rome and Milan.
#[cfg(any(feature = "naples", feature = "rome", feature = "milan"))]
pub mod sp3;
