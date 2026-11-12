// SPDX-License-Identifier: MIT OR Apache-2.0
//! AMD family flows: the PSP-booted ("noncar") SoCs from Family 17h on
//! ([`noncar`]) start in PSP-trained DRAM, never Intel CAR. Each SoC module
//! implements the flow's [`noncar::Soc`].
#![no_std]
extern crate alloc;
#[cfg(feature = "noncar")]
pub mod acpi;

/// How the FFS stores what this platform's stages load. The host plan
/// compresses each image with its constant and the stage loading it passes
/// `codec` of the same constant, so a stage links only the decoders it uses.
pub mod compression {
    use fstart_core::Compression;
    /// The payload's files, loaded by the stage.
    pub const PAYLOAD: Compression = Compression::Lz4;
}

/// The boot media mount every AMD flow shares.
#[cfg(feature = "media")]
pub mod media;
#[cfg(feature = "noncar")]
pub mod noncar;
#[cfg(feature = "picasso")]
pub mod picasso;
#[cfg(feature = "zeppelin")]
pub mod zeppelin;
/// SMM image ABI and handler binding: a board's `smm.rs` binds its handler
/// with `fstart_platform_amd::smm::smm_bin!`.
#[cfg(feature = "smm")]
pub use fstart_smm as smm;
#[cfg(feature = "bundle")]
#[doc(hidden)]
pub use fstart_stage as __stage;
/// A board's host adapter: `Plan::<Board>::emit`.
#[cfg(all(feature = "host", feature = "noncar"))]
pub use noncar::host::Plan;
