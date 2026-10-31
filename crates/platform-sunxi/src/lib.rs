//! Fixed Allwinner sunxi platform flows.
//!
//! Board policy is static POD data. Each SoC exports the phases of its SRAM
//! bootblock and its DRAM stage; the board's `main`s call them.

#![no_std]

#[cfg(feature = "stage")]
extern crate ufmt;

#[cfg(feature = "stage")]
mod boot;
#[cfg(all(
    feature = "stage",
    target_arch = "arm",
    any(feature = "a20", feature = "h3")
))]
pub mod fel;
#[cfg(feature = "stage")]
mod stage;
#[cfg(all(
    feature = "stage",
    feature = "linux",
    any(feature = "a20", feature = "h3")
))]
pub use stage::LinuxPayload;
#[cfg(all(feature = "stage", any(feature = "a20", feature = "h3")))]
pub use stage::{BootMedia, DramStage};

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

/// Declare a board's stage `main`. The SRAM bootblock's `main` takes no
/// arguments: `entry!(main);`. The DRAM stage's `main` takes the handoff
/// address the bootblock passed: `entry!(main, handoff);`.
#[macro_export]
macro_rules! entry {
    ($main:path) => { $crate::stage_runtime::stage_bin!(main: $main); };
    ($main:path, handoff) => { $crate::stage_runtime::stage_bin!(main_handoff: $main); };
}
