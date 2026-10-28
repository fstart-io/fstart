// SPDX-License-Identifier: MIT OR Apache-2.0
//! Flat AMD microcode patches as the BIOS directory (type 0x66 entries) and
//! the FFS carry them: concatenated, without a container. A patch's size
//! follows from the CPU it is for, which its header names (the equivalence
//! ID at +0x18), so one blob can carry the patches of several generations.

/// Offset of the processor revision (equivalence) ID in a patch header.
const EQUIVALENCE_ID: usize = 0x18;

/// The patch size for CPUs of an equivalence ID, `None` for families whose
/// patch format is not known here.
pub const fn patch_size(equivalence_id: u16) -> Option<usize> {
    // Bits 15:12 are the extended family, 11:8 the extended model.
    match equivalence_id >> 8 {
        // Family 17h: Zeppelin, Raven/Picasso, Rome, ...
        0x80..=0x8f => Some(0xc80),
        // Family 19h models 00h-0Fh: Milan.
        0xa0 => Some(0x15c0),
        _ => None,
    }
}

/// The equivalence ID a patch is for.
pub fn equivalence_id(patch: &[u8]) -> Option<u16> {
    patch
        .get(EQUIVALENCE_ID..EQUIVALENCE_ID + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
}

/// The patches of a blob, in order. `Err(offset)` for a patch whose size is
/// unknown or that runs past the end; the walk stops there.
pub fn patches(blob: &[u8]) -> impl Iterator<Item = Result<&[u8], usize>> {
    let mut offset = 0;
    core::iter::from_fn(move || {
        let rest = blob.get(offset..).filter(|rest| !rest.is_empty())?;
        let patch = equivalence_id(rest)
            .and_then(patch_size)
            .and_then(|size| rest.get(..size));
        match patch {
            Some(patch) => {
                offset += patch.len();
                Some(Ok(patch))
            }
            None => {
                let at = offset;
                offset = blob.len();
                Some(Err(at))
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec::Vec;

    fn patch(id: u16, size: usize) -> Vec<u8> {
        let mut p = std::vec![0u8; size];
        p[EQUIVALENCE_ID..EQUIVALENCE_ID + 2].copy_from_slice(&id.to_le_bytes());
        p
    }

    #[test]
    fn walks_patches_of_several_generations() {
        let blob = [
            patch(0x8310, 0xc80),
            patch(0xa011, 0x15c0),
            patch(0x8012, 0xc80),
        ]
        .concat();
        let ids: Vec<_> = patches(&blob)
            .map(|p| equivalence_id(p.unwrap()).unwrap())
            .collect();
        assert_eq!(ids, [0x8310, 0xa011, 0x8012]);
        assert!(patches(&[]).next().is_none());
    }

    #[test]
    fn stops_at_an_unknown_or_truncated_patch() {
        let blob = [patch(0x8310, 0xc80), patch(0xa120, 0x100)].concat();
        let walk: Vec<_> = patches(&blob).map(|p| p.map(<[u8]>::len)).collect();
        assert_eq!(walk, [Ok(0xc80), Err(0xc80)]);
        let short = patch(0xa011, 0x1000);
        assert_eq!(patches(&short).collect::<Vec<_>>(), [Err(0)]);
    }
}
