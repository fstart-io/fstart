//! Fixed Allwinner sunxi platform flows.
//!
//! Board policy is static POD data. The A20 SRAM and DRAM flows are
//! handwritten; boards only supply that policy and optional hook code.

#![no_std]

#[cfg(feature = "stage")]
extern crate ufmt;

pub mod a20;
pub mod d1;
pub mod egon;
pub mod h3;
