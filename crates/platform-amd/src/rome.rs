// SPDX-License-Identifier: MIT OR Apache-2.0
//! AMD Rome (EPYC 7002) in the SP3 socket: an SSP I/O die and CCDs
//! ([`crate::iod`]).

/// The SSP die.
pub enum Ssp {}

impl crate::iod::Variant for Ssp {
    const NAME: &'static str = "rome";
    const SILICON: fstart_driver_amd::cpu::Silicon = fstart_driver_amd::cpu::Silicon::Ssp;
    /// SSP-A0 is model 30h, SSP-B0 model 31h.
    const SIGNATURES: &'static [u32] = &[0x0083_0f00, 0x0083_0f10];
}

/// The Rome SoC.
pub type Rome = crate::iod::Iod<Ssp>;
