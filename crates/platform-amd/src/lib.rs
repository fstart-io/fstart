// SPDX-License-Identifier: MIT OR Apache-2.0
//! The AMD SoCs and their boot flow. Every SoC from Family 17h on is
//! PSP-booted: it starts in PSP-trained DRAM, never Intel CAR. A SoC
//! composes the driver's IP blocks into its silicon init ([`soc::Init`]),
//! which the phases of the flow ([`flow`]) call. What the flow needs of a
//! SoC beyond that is [`flow::Soc`]: the SoCs made of dies share theirs
//! ([`noncar`]; [`picasso`], [`zeppelin`]), the SP3 socket has its own
//! ([`sp3`]: Naples, Rome, Milan).
#![cfg_attr(not(any(test, feature = "host")), no_std)]
extern crate alloc;
/// The ACPI tables, for every SoC.
pub mod acpi;

/// How the FFS stores what this platform's stages load. The host plan
/// compresses each image with its constant and the stage loading it passes
/// `codec` of the same constant, so a stage links only the decoders it uses.
pub mod compression {
    use fstart_core::Compression;
    /// The payload's files, loaded by the stage.
    pub const PAYLOAD: Compression = Compression::Lz4;
}

/// The boot flow, for every SoC.
pub mod flow;
/// The reset image's DRAM layout and boot flash, for every SoC.
pub mod layout;
/// The boot media mount every AMD flow shares.
#[cfg(feature = "media")]
pub mod media;
/// The memory map, for every SoC.
pub mod memory;
/// MP init, for every flow.
pub mod mp;
pub mod noncar;
pub mod picasso;
/// The SMBIOS tables, for every SoC.
pub mod smbios;
/// The SoCs made of dies: their IP blocks and the order they come up in.
pub mod soc;
/// The SP3 socket: Naples, Rome and Milan.
pub mod sp3;
pub mod zeppelin;
/// SMM image ABI and handler binding: a board's `smm.rs` binds its handler
/// with `fstart_platform_amd::smm::smm_bin!`.
#[cfg(feature = "smm")]
pub use fstart_smm as smm;
#[cfg(feature = "bundle")]
#[doc(hidden)]
pub use fstart_stage as __stage;

/// Declare the board's stage `main`: `fstart_platform_amd::entry!(main);`
#[macro_export]
macro_rules! entry {
    ($main:path) => {
        $crate::__stage::stage_bin!(main: $main);
    };
}
/// The `--option`s of every AMD board's host plan: where the PSP
/// firmware comes from and the direct x86 Linux launcher's settings.
#[cfg(feature = "host")]
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct HostOptions {
    /// The vendor flash image the PSP firmware is taken from or composed
    /// into (`--image-config`).
    pub image_config: Option<String>,
    /// The directory the PSP firmware is packed from (`--blobs`, or
    /// extracted by fbuild from `--donor`).
    pub blobs: Option<String>,
    #[serde(flatten)]
    pub x86_linux: fstart_image_build::plan::X86LinuxOptions,
}

/// A die SoC board's host adapter: the `HostPlan` adapter. An SP3 board's
/// is [`sp3::Plan`].
#[cfg(feature = "host")]
pub use noncar::host::Plan;
