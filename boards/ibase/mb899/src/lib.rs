//! Experimental IBASE MB899: i945GM/ICH7-M and W83627EHG policy.
//! Protected-mode stages support Socket M Core Duo and Core 2 configurations.
#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod config;
pub mod mainboard;
#[cfg(feature = "smm")]
pub mod smm;
pub struct Board;
pub use config::*;
