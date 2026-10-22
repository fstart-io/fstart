// SPDX-License-Identifier: MIT OR Apache-2.0
//! SMM: the board's FCH SMM handler in
//! TSEG at the top of the DRAM below 4 GiB, every CPU's SMBASE in it and SMM
//! locked, then the FCH's SMI sources enabled.
use fstart_arch::x86::{
    cpu::amd::smm::{AmdSmm, InstalledTseg},
    mp::MpHandle,
};
use fstart_driver_amd::fch::smi::FchSmi;

/// The board's SMM image that fbuild built from its `smm.rs`.
#[cfg(fstart_amd_has_smm_image)]
const IMAGE: Option<&[u8]> = Some(include_bytes!(env!("FSTART_SMM_IMAGE")));
#[cfg(not(fstart_amd_has_smm_image))]
const IMAGE: Option<&[u8]> = None;

/// Install the board's SMM handler in the TSEG at `base`; `name` is the
/// log prefix.
pub fn install(
    name: &'static str,
    mp: &MpHandle,
    smi: &FchSmi,
    base: u64,
    size: u32,
) -> Result<InstalledTseg, &'static str> {
    IMAGE
        .ok_or("no SMM image")
        .and_then(|image| {
            AmdSmm::new(name, smi)
                .install(mp, image, base, size)
                .map_err(|error| error.name())
        })
        .map_err(|error| {
            fstart_log::error!("{}: SMM: {}", name, error);
            "SMM installation"
        })
}
