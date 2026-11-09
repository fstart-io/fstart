//! Fixed SpacemiT platform flows.
//!
//! K3: the boot ROM loads the first stage into on-chip SRAM from a block
//! device or over USB fastboot and enters it in M-mode on hart 0.

#![no_std]

pub mod facts;
#[cfg(feature = "host")]
pub mod host;
#[cfg(all(feature = "stage", feature = "k3", target_arch = "riscv64"))]
pub mod k3;

#[cfg(feature = "stage")]
#[doc(hidden)]
pub use fstart_stage as stage_runtime;
#[cfg(feature = "host")]
pub use host::Plan;

/// Declare a board's stage `main`: `entry!(main);`.
#[macro_export]
macro_rules! entry {
    ($main:path) => { $crate::stage_runtime::stage_bin!(main: $main); };
}
