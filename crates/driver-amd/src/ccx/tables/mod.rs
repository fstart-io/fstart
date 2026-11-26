// SPDX-License-Identifier: MIT OR Apache-2.0
//! The vendor CCX reset MSR tables of each die (the [`super::table`]
//! records), generated from the donor images: ZP (AmdCcxZenZp, Zeppelin
//! and Naples), SSP (AmdCcxVh, Rome) and GN (AmdCcxZen3, Milan); RS
//! (AmdCcxZen4) and WH (openSIL SilVenicePei) run theirs from their own
//! `thread_init` ([`super::rs`], [`super::wh`]).
use super::table::Record;
use crate::cpu::Silicon;

pub mod gn;
pub mod rs;
pub mod ssp;
pub mod wh;
pub mod zp;

/// The table of `silicon`'s cores.
pub const fn records(silicon: Silicon) -> &'static [Record] {
    match silicon {
        Silicon::Zp => zp::RECORDS,
        Silicon::Ssp => ssp::RECORDS,
        Silicon::Gn => gn::RECORDS,
    }
}
