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
#[cfg(feature = "rs-acpi")]
pub mod rs_acpi;
#[cfg(all(feature = "rs", target_os = "none", feature = "rs-stage"))]
pub mod rs_cpu;
#[cfg(all(feature = "rs", target_os = "none", feature = "rs-stage"))]
pub mod rs_discovery;
#[cfg(all(
    feature = "rs",
    any(test, all(target_os = "none", feature = "rs-stage"))
))]
pub mod rs_fch;
#[cfg(all(feature = "rs", feature = "host"))]
pub mod rs_host;
#[cfg(all(
    feature = "rs",
    any(test, all(target_os = "none", feature = "rs-stage"))
))]
pub mod rs_io;
#[cfg(all(feature = "rs-acpi", target_os = "none", feature = "rs-stage"))]
pub mod rs_ioapic;
#[cfg(all(feature = "rs", target_os = "none", feature = "rs-stage"))]
pub mod rs_iommu;
#[cfg(all(feature = "rs-linux", target_os = "none", feature = "rs-stage"))]
pub mod rs_linux;
#[cfg(all(feature = "rs", target_os = "none", feature = "rs-stage"))]
pub mod rs_mp;
#[cfg(all(feature = "rs", target_os = "none", feature = "rs-stage"))]
pub mod rs_mpio;
#[cfg(all(feature = "rs", target_os = "none", feature = "rs-stage"))]
pub mod rs_nbio;
#[cfg(feature = "rs-acpi")]
pub mod rs_numa;
#[cfg(all(feature = "rs", target_os = "none", feature = "rs-stage"))]
pub mod rs_pci;
#[cfg(all(feature = "rs", target_os = "none", feature = "rs-stage"))]
pub mod rs_psp;
#[cfg(feature = "rs")]
pub mod rs_resources;
#[cfg(feature = "rs-acpi")]
pub mod rs_smbios;
#[cfg(feature = "rs-smi")]
pub mod rs_smm;
#[cfg(all(feature = "rs-usb", target_os = "none", feature = "rs-stage"))]
pub mod rs_usb;
#[cfg(feature = "host")]
pub mod smm_unit;
#[cfg(feature = "zeppelin")]
pub mod zeppelin;
/// SMM image ABI and handler binding: a board's `smm.rs` binds its handler
/// with `fstart_platform_amd::smm::smm_bin!`.
#[cfg(any(feature = "smm", feature = "rs-smm"))]
pub use fstart_smm as smm;
#[cfg(feature = "bundle")]
#[doc(hidden)]
pub use fstart_stage as __stage;
/// A board's host adapter: `Plan::<Board>::emit`.
#[cfg(all(feature = "host", feature = "noncar"))]
pub use noncar::host::Plan;
