// SPDX-License-Identifier: MIT OR Apache-2.0
//! The board's SMM handler in TSEG, for every SoC: every CPU's SMBASE in
//! it and SMM locked, then the SoC's SMI sources enabled
//! ([`fstart_arch::x86::cpu::amd::smm::SmiControl`]).
use fstart_arch::x86::{
    cpu::amd::smm::{AmdSmm, InstalledTseg},
    mp::MpHandle,
};

/// The board's SMM image that fbuild built from its `smm.rs` (the host
/// plan's `smm` unit; `build.rs` checked that it fits TSEG).
#[cfg(fstart_amd_has_smm_image)]
pub const IMAGE: Option<&[u8]> = Some(include_bytes!(env!("FSTART_SMM_IMAGE")));
#[cfg(not(fstart_amd_has_smm_image))]
pub const IMAGE: Option<&[u8]> = None;

/// Install the board's SMM handler in the TSEG at `base`, with `smi` as
/// the owner of the SMI sources; `name` is the log prefix.
pub fn install(
    name: &'static str,
    mp: &MpHandle,
    smi: &impl fstart_arch::x86::cpu::amd::smm::SmiControl,
    base: u64,
    size: u32,
) -> Result<InstalledTseg, &'static str> {
    IMAGE
        .ok_or("no SMM image")
        .and_then(|image| {
            crate::tpm::measure_smm(image);
            AmdSmm::new(name, smi)
                .install(mp, image, base, size)
                .map_err(|error| error.name())
        })
        .map_err(|error| {
            fstart_log::error!("{}: SMM: {}", name, error);
            "SMM installation"
        })
}
