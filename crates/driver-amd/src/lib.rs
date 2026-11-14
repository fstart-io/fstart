// SPDX-License-Identifier: MIT OR Apache-2.0
//! AMD hardware mechanisms. No board discovery, stage loading or FSP calls.
//!
//! The modules are IP blocks, each with its common code and a submodule
//! per IP version where the code differs. Which versions a SoC is made of
//! and in which order they come up is the platform's; [`soc::Soc`] is what
//! the blocks need to know of it. The register transports are [`smn`] and
//! [`df`].
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
pub mod smn;
pub mod smu;
pub mod soc;
pub mod table;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod window;
