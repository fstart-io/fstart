// SPDX-License-Identifier: MIT OR Apache-2.0
//! AMD Rome (EPYC 7002) in the SP3 socket: an SSP I/O die and CCDs
//! ([`crate::iod`]).

/// The SSP die.
pub enum Ssp {}

impl crate::iod::Variant for Ssp {
    const NAME: &'static str = "rome";
    const SILICON: fstart_driver_amd::cpu::Silicon = fstart_driver_amd::cpu::Silicon::Ssp;
}

/// The Rome SoC.
pub type Rome = crate::iod::Iod<Ssp>;
