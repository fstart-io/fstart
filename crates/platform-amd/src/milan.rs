// SPDX-License-Identifier: MIT OR Apache-2.0
//! AMD Milan (EPYC 7003) in the SP3 socket: a GN I/O die and CCDs
//! ([`crate::iod`]).

/// The GN die.
pub enum Gn {}

impl crate::iod::Variant for Gn {
    const NAME: &'static str = "milan";
    const SILICON: fstart_driver_amd::cpu::Silicon = fstart_driver_amd::cpu::Silicon::Gn;
    /// GN-A0 is model 0, GN-B0/B1/B2 model 1.
    const SIGNATURES: &'static [u32] = &[0x00a0_0f00, 0x00a0_0f10];
}

/// The Milan SoC.
pub type Milan = crate::iod::Iod<Gn>;
