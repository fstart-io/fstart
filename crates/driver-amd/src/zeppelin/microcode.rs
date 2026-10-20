// SPDX-License-Identifier: MIT OR Apache-2.0
//! Zeppelin microcode: the vendor ROM's BIOS directory type 0x66 entries
//! (0xc80 bytes each, one per equivalence ID 8000/8010/8011/8012), loaded on
//! every core. The platform packages them.
use super::Zeppelin;
use crate::fam17h::microcode::{self, Family};
pub use crate::fam17h::microcode::{Error, Outcome, PATCH_BYTES, equivalent_revision};

impl Family for Zeppelin {
    /// ZP-A0 is model 0, ZP-B0/B1/B2 are model 1.
    const SIGNATURES: &'static [u32] = &[0x0080_0f00, 0x0080_0f10];
    const MAX_PATCHES: usize = 4;
}
pub type Bundle<'a> = microcode::Bundle<'a, Zeppelin>;
pub type Patch<'a> = microcode::Patch<'a, Zeppelin>;

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    #[test]
    fn selects_the_vendor_patch_by_equivalence_id() {
        // Header prefixes of the MJ11 vendor ROM's four 0x66 entries.
        let patches: Vec<u8> = [
            (0x0800_1250, 0x8012),
            (0x0800_1129, 0x8011),
            (0x0800_100c, 0x8010),
            (0x0800_002a, 0x8000),
        ]
        .into_iter()
        .flat_map(|(id, revision): (u32, u16)| {
            let mut patch = [0; PATCH_BYTES];
            patch[4..8].copy_from_slice(&id.to_le_bytes());
            patch[24..26].copy_from_slice(&revision.to_le_bytes());
            patch
        })
        .collect();
        let bundle = Bundle::parse(&patches).unwrap();
        // EPYC 3151: ZP-B2.
        assert_eq!(bundle.select(0x0080_0f12).unwrap().revision(), 0x0800_1250);
        assert_eq!(bundle.select(0x0080_0f00).unwrap().revision(), 0x0800_002a);
        assert_eq!(bundle.select(0x0080_0f13).err(), Some(Error::CpuIdentity));
        assert_eq!(bundle.select(0x0081_0f10).err(), Some(Error::CpuIdentity));
    }
}
