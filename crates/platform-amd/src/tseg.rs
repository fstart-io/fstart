// SPDX-License-Identifier: MIT OR Apache-2.0
//! The board's SMM handler in TSEG, for every SoC: every CPU's SMBASE in
//! it and SMM locked, then the SoC's SMI sources enabled
//! ([`fstart_arch::x86::cpu::amd::smm::SmiControl`]).
use fstart_arch::x86::{
    cpu::amd::smm::{AmdSmm, InstalledTseg},
    mp::MpHandle,
};

/// Install the board's SMM handler in the TSEG at `base`, with `smi` as
/// the owner of the SMI sources; `name` is the log prefix.
pub fn install(
    name: &'static str,
    mp: &MpHandle,
    smi: &impl fstart_arch::x86::cpu::amd::smm::SmiControl,
    base: u64,
    size: u32,
) -> Result<InstalledTseg, &'static str> {
    fstart_core::services::ffs_context::read_verified_asset(fstart_core::ffs::SMM_IMAGE_ASSET)
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
