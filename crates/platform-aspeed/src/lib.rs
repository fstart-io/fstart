//! Fixed ASPEED BMC SoC platform flows.
//!
//! The AST2400 and AST2500 ARM cores execute SPI flash in place from the
//! reset vector; [`head`] builds the vector table and hardware strap block
//! that open such an image.

#![no_std]

#[cfg(feature = "stage")]
extern crate ufmt;

#[cfg(all(feature = "stage", feature = "ast2500", target_arch = "arm"))]
pub mod ast2500;
pub mod facts;
pub mod head;
#[cfg(feature = "host")]
pub mod host;
pub mod layout;

#[cfg(feature = "stage")]
#[doc(hidden)]
pub use fstart_stage as stage_runtime;
#[cfg(feature = "host")]
pub use host::Plan;

/// Declare a board's stage `main`: `entry!(main);`, or `entry!(main, handoff);`
/// for a stage whose `main` takes its predecessor's handoff address.
#[macro_export]
macro_rules! entry {
    ($main:path) => { $crate::stage_runtime::stage_bin!(main: $main); };
    ($main:path, handoff) => { $crate::stage_runtime::stage_bin!(main_handoff: $main); };
}
