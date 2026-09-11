//! Fixed Allwinner sunxi platform flows.
//!
//! Board policy is static POD data. The A20 SRAM and DRAM flows are
//! handwritten; boards only supply that policy and optional hook code.

#![no_std]

#[cfg(feature = "stage")]
extern crate ufmt;

#[cfg(feature = "stage")]
mod boot;

pub mod a20;
pub mod d1;
pub mod egon;
pub mod facts;
pub mod h3;
#[cfg(feature = "host")]
pub mod host;

#[cfg(feature = "stage")]
#[doc(hidden)]
pub use fstart_stage as stage_runtime;
#[cfg(feature = "host")]
pub use host::Plan;

/// Hygienic firmware entry; dependency names stay inside the platform.
#[macro_export]
macro_rules! stage_bin {
    ($program:ty) => { $crate::stage_runtime::stage_bin!(program: $program); };
}
