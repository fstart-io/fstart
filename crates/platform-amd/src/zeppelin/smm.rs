// SPDX-License-Identifier: MIT OR Apache-2.0
//! SMM on Zeppelin: the board's FCH SMM
//! handler in an 8 MiB TSEG at the top of the DRAM below 4 GiB, every CPU's
//! SMBASE in it and SMM locked, then the FCH's SMI sources enabled.
use super::{Board, memory::TSEG_SIZE};
use fstart_arch::x86::{
    cpu::amd::smm::{AmdSmm, InstalledTseg},
    mp::MpHandle,
};
use fstart_driver_amd::{fch::smi::FchSmi, zeppelin::fch};

/// The board's SMM image that fbuild built from its `smm.rs`.
#[cfg(fstart_amd_has_smm_image)]
const IMAGE: Option<&[u8]> = Some(include_bytes!(env!("FSTART_SMM_IMAGE")));
#[cfg(not(fstart_amd_has_smm_image))]
const IMAGE: Option<&[u8]> = None;

pub fn install<B: Board>(mp: &MpHandle, tseg_base: u64) -> Result<InstalledTseg, &'static str> {
    let image = IMAGE.ok_or("no SMM image")?;
    // SAFETY: BSP only after MP init; ACPI MMIO and the ACPI blocks are
    // decoded since the FCH init.
    let smi = unsafe { FchSmi::new(fch::ACPI_PORTS, B::SMI_GEVENTS) };
    AmdSmm::new("zeppelin", &smi)
        .install(mp, image, tseg_base, TSEG_SIZE)
        .map_err(|error| error.name())
}
