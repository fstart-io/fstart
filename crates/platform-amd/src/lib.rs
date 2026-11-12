// SPDX-License-Identifier: MIT OR Apache-2.0
//! AMD family flows: the PSP-booted ("noncar") SoCs from Family 17h on
//! ([`noncar`]) start in PSP-trained DRAM, never Intel CAR. The silicon
//! init of a SoC is its driver's (`fstart_driver_amd::soc::Init`), which
//! the flow's phases call; a SoC module here adds the reset image's layout,
//! the host plan and the SoC's tables ([`noncar::Soc`]).
#![cfg_attr(not(any(test, feature = "host")), no_std)]
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
/// MP init, for every flow.
#[cfg(any(feature = "noncar", feature = "sp3"))]
pub mod mp;
#[cfg(feature = "noncar")]
pub mod noncar;
#[cfg(feature = "picasso")]
pub mod picasso;
/// The SP3 socket: Naples, Rome and Milan.
#[cfg(feature = "sp3")]
pub mod sp3;
#[cfg(feature = "zeppelin")]
pub mod zeppelin;
/// SMM image ABI and handler binding: a board's `smm.rs` binds its handler
/// with `fstart_platform_amd::smm::smm_bin!`.
#[cfg(feature = "smm")]
pub use fstart_smm as smm;
#[cfg(any(feature = "bundle", feature = "bundle-stage"))]
#[doc(hidden)]
pub use fstart_stage as __stage;

/// Declare the board's stage `main`: `fstart_platform_amd::entry!(main);`
#[macro_export]
macro_rules! entry {
    ($main:path) => {
        $crate::__stage::stage_bin!(main: $main);
    };
}
/// A die SoC board's host adapter: `Plan::<Board>::emit`. An SP3 board's
/// is [`sp3::Plan`].
#[cfg(all(feature = "host", feature = "noncar"))]
pub use noncar::host::Plan;
