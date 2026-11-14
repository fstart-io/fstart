// SPDX-License-Identifier: MIT OR Apache-2.0
//! AMD Milan (EPYC 7003) in the SP3 socket: a GN I/O die and CCDs
//! ([`crate::iod`]).

/// The GN die.
pub enum Gn {}

impl crate::iod::Variant for Gn {
    const NAME: &'static str = "milan";
    const SILICON: fstart_driver_amd::cpu::Silicon = fstart_driver_amd::cpu::Silicon::Gn;
}

/// The Milan SoC.
pub type Milan = crate::iod::Iod<Gn>;
