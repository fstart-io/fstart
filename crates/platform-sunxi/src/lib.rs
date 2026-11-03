//! Fixed Allwinner sunxi platform flows.
//!
//! Board policy is static POD data. Each SoC exports the phases of its SRAM
//! bootblock and its DRAM stage; the board's `main`s call them.

#![no_std]

#[cfg(feature = "stage")]
extern crate ufmt;

#[cfg(feature = "stage")]
mod boot;

/// How the FFS stores what this platform's stages load. The host plan
/// compresses each image with its constant and the stage loading it passes
/// `codec` of the same constant, so a stage links only the decoders it uses.
pub mod compression {
    use fstart_core::Compression;
    /// The DRAM stage, loaded by the SRAM bootblock: stored, so the
    /// bootblock links no decoder.
    pub const MAIN_STAGE: Compression = Compression::None;
    /// The payload's files, loaded by the DRAM stage. Stored: LZ4 through
    /// the block-backed boot media reads byte by byte and is far slower
    /// than the larger flat read.
    pub const PAYLOAD: Compression = Compression::None;
}

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
