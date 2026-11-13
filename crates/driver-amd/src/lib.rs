// SPDX-License-Identifier: MIT OR Apache-2.0
//! AMD hardware mechanisms. No board discovery, stage loading or FSP calls.
//!
//! The modules are IP blocks, each with its common code and a submodule
//! per IP version where the code differs; [`soc`] says which versions a SoC
//! is made of. The register transports are [`smn`] and [`df`].
#![cfg_attr(not(test), no_std)]
pub mod acp;
pub mod apob;
pub mod ccx;
pub mod cpu;
pub mod df;
pub mod dxio;
pub mod fch;
pub mod nbio;
pub mod psp;
#[cfg(test)]
mod silicon_tests;
pub mod smn;
pub mod smu;
pub mod soc;
/// The SP3 server silicon: Naples, Rome and Milan.
pub mod sp3;
pub mod table;
pub mod window;
