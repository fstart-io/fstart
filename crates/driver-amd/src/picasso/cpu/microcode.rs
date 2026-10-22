// SPDX-License-Identifier: MIT OR Apache-2.0
//! Picasso PCO B0/B1 and RV2 A0 microcode, through the shared Family 17h
//! flat-patch loader.
use crate::fam17h::microcode::{self, Family};
pub use crate::fam17h::microcode::{Error, Outcome, PATCH_BYTES, equivalent_revision};

pub enum Picasso {}
impl Family for Picasso {
    const SIGNATURES: &'static [u32] = &[0x00810f10, 0x00810f80, 0x00820f00];
    const MAX_PATCHES: usize = 3;
}
/// The three vendor .bin inputs for PCO B0/B1 plus RV2 A0.
pub const MAX_PATCHES: usize = Picasso::MAX_PATCHES;
pub type Bundle<'a> = microcode::Bundle<'a, Picasso>;
pub type Patch<'a> = microcode::Patch<'a, Picasso>;
