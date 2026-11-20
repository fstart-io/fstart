//! Import a raw Intel descriptor-mode flash dump. The board plan is
//! the expected geometry, not a substitute for parsing the donor's descriptor.
use crate::bundle::sha256;
use fstart_core::{IntelIfdFlashLayout, IntelIfdRegion as Kind, memory::IfdVersion};
use serde::Serialize;
use std::ops::Range;
use zerocopy::{FromBytes, little_endian::U32};

#[derive(Serialize)]
pub(crate) struct ImportedRegion {
    region: &'static str,
    offset: u32,
    size: u32,
    sha256: String,
}

/// Compare the complete physical region map with the board layout. As in
/// coreboot's ifdtool, the platform selects the version (not the donor).
pub(crate) fn validate(layout: &IntelIfdFlashLayout, rom: &[u8]) -> Result<Range<usize>, String> {
    layout.check()?;
    if layout
        .regions
        .as_slice()
        .iter()
        .any(|r| r.kind.flreg_index().is_none())
    {
        return Err("cannot validate an unnumbered Intel flash region".into());
    }
    if rom.len() != layout.size() as usize {
        return Err(format!(
            "Intel flash: expected {:#x} bytes, got {:#x}",
            layout.size(),
            rom.len()
        ));
    }
    let fd = rom.get(..4096).ok_or("missing Intel flash descriptor")?;
    let word = |offset: usize| -> Result<u32, String> {
        let (value, _) =
            U32::read_from_prefix(fd.get(offset..).ok_or("IFD offset outside descriptor")?)
                .map_err(|_| "truncated IFD word")?;
        Ok(value.get())
    };
    const SIGNATURE: u32 = 0x0ff0_a55a;
    let header = match (word(0)? == SIGNATURE, word(16)? == SIGNATURE) {
        (true, false) => 0,
        (false, true) => 16,
        _ => {
            return Err(
                "expected one Intel descriptor signature at 0 or 0x10 in a raw flash image".into(),
            );
        }
    };
    let map = word(header + 4)?;
    let nc = (map >> 8) & 3;
    if nc > 1 {
        return Err("unsupported IFD component count".into());
    }
    let fcba = ((map & 0xff) << 4) as usize;
    let frba = (((map >> 16) & 0xff) << 4) as usize;
    let map1 = word(header + 8)?;
    let fmba = ((map1 & 0xff) << 4) as usize;
    let straps = (((map1 >> 16) & 0xff) << 4) as usize;
    // The component, master, region and strap sections need not be in
    // order. FLUMAP is always at 0xefc, independent of signature offset.
    const UPPER_MAP: usize = 0xefc;
    let sections = [fcba, frba, fmba, straps];
    for (i, &base) in sections.iter().enumerate() {
        if i == 3 && base == 0 {
            continue;
        } // no strap section
        if base < header + 16 || base >= UPPER_MAP || sections[..i].contains(&base) {
            return Err("overlapping or invalid IFD section bases".into());
        }
    }
    let next = |at| {
        sections
            .iter()
            .copied()
            .filter(|b| *b > at)
            .min()
            .unwrap_or(UPPER_MAP)
    };
    let available = (next(frba) - frba) / 4;
    // NR is reserved on newer PCHs. Bound FRBA by the next descriptor
    // section as ifdtool does, not by the full 4 KiB FD nor a fixed 16.
    let count = match layout.version {
        IfdVersion::V1 => {
            let nr = ((map >> 24) & 7) as usize;
            if !(1..=4).contains(&nr) || nr + 1 > available {
                return Err("invalid legacy IFD region count".into());
            }
            nr + 1
        }
        IfdVersion::V1_5 | IfdVersion::V2 => available.min(Kind::REGISTERS.len()),
    };
    let (density_bits, page_mask) = match layout.version {
        IfdVersion::V1 => (3, 0xfff),
        IfdVersion::V1_5 => (4, 0xfff),
        IfdVersion::V2 => (4, 0x7fff),
    };
    let component = word(fcba)?;
    let mut capacity = 0usize;
    for chip in 0..=nc {
        let density = (component >> (chip * density_bits)) & ((1 << density_bits) - 1);
        if density > 7 {
            return Err("invalid density for an installed IFD flash component".into());
        }
        capacity += (512 << 10) << density;
    }
    if capacity != rom.len() {
        return Err(format!(
            "IFD component capacity {capacity:#x} does not match the flash image"
        ));
    }
    for (index, kind) in Kind::REGISTERS.into_iter().enumerate() {
        if index >= count {
            if layout.regions.as_slice().iter().any(|r| r.kind == kind) {
                return Err(format!("IFD lacks expected {} region", kind.as_str()));
            }
            continue;
        }
        let reg = word(frba + index * 4)?;
        let first = reg & page_mask;
        let last = (reg >> 16) & page_mask;
        // Ignore reserved encoding bits only for disabled entries. Never
        // truncate a larger active region to manufacture a layout match.
        let actual = if reg == u32::MAX || first > last {
            None
        } else {
            if layout.version == IfdVersion::V1 && reg & 0xf000_f000 != 0 {
                return Err(format!("unsupported active IFD FLREG{index}"));
            }
            Some((first << 12, (last - first + 1) << 12))
        };
        let expected = layout
            .regions
            .as_slice()
            .iter()
            .find(|r| r.kind == kind)
            .map(|r| (r.offset, r.size));
        if actual != expected || (kind == Kind::Descriptor && actual != Some((0, 4096))) {
            return Err(format!(
                "IFD {} region mismatch: expected {expected:#x?}, found {actual:#x?}",
                kind.as_str()
            ));
        }
        if let Some((offset, size)) = actual
            && matches!(kind, Kind::Me | Kind::Gbe)
        {
            let region = &rom[offset as usize..(offset + size) as usize];
            if region.iter().all(|b| *b == 0xff) || region.iter().all(|b| *b == 0) {
                return Err(format!(
                    "empty {} region in Intel flash image",
                    kind.as_str()
                ));
            }
        }
    }
    let bios = layout.bios_region().ok_or("IFD lacks BIOS")?;
    Ok(bios.offset as usize..(bios.offset + bios.size) as usize)
}

pub(crate) fn compose(
    layout: &IntelIfdFlashLayout,
    vendor: &[u8],
    assembled: &[u8],
) -> Result<(Vec<u8>, Vec<ImportedRegion>), String> {
    let bios = validate(layout, vendor)?;
    if assembled.len() != vendor.len() {
        return Err("assembled Intel flash size mismatch".into());
    }
    let mut merged = vendor.to_vec();
    merged[bios.clone()].copy_from_slice(&assembled[bios]);
    let regions = layout
        .regions
        .as_slice()
        .iter()
        .filter(|r| r.kind != Kind::Bios)
        .map(|r| ImportedRegion {
            region: r.kind.as_str(),
            offset: r.offset,
            size: r.size,
            sha256: sha256(&vendor[r.offset as usize..(r.offset + r.size) as usize]),
        })
        .collect();
    Ok((merged, regions))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::IfdTransport;
    fn fixture() -> (IntelIfdFlashLayout, Vec<u8>) {
        let layout = IfdTransport(vec![
            (Kind::Descriptor, 0, 4096),
            (Kind::Gbe, 4096, 8192),
            (Kind::Bios, 0x40000, 0x40000),
        ])
        .decode()
        .unwrap();
        let mut rom = vec![0x69; 0x80000];
        for (at, word) in [
            (0x10, 0x0ff0_a55au32),
            (0x14, 0x0304_0003),
            (0x18, 6),
            (0x30, 0),
            (0x40, 0),
            (0x44, 0x007f_0040),
            (0x48, 0x1fff),
            (0x4c, 0x0002_0001),
            (0x50, u32::MAX), // erased padding after FLREG3, as on T61
        ] {
            rom[at..at + 4].copy_from_slice(&word.to_le_bytes());
        }
        (layout, rom)
    }
    #[test]
    fn replaces_only_bios_including_preservation_of_unclaimed_gaps() {
        let (layout, rom) = fixture();
        let (out, regions) = compose(&layout, &rom, &vec![7; rom.len()]).unwrap();
        assert_eq!(regions.len(), 2);
        assert_eq!(&out[..0x40000], &rom[..0x40000]);
        assert!(out[0x40000..].iter().all(|b| *b == 7));
    }
    // Only map registers, no vendor bytes. The 32 MiB Tioga Pass geometry
    // can be compared with `ifdtool -p lbg -d` on coreboot's descriptor.
    #[test]
    fn lewisburg_15bit_addresses_and_high_numbered_regions() {
        let regions = IfdTransport(vec![
            (Kind::Descriptor, 0, 0x1000),
            (Kind::Gbe, 0x1000, 0x2000),
            (Kind::Me, 0x3000, 0xa23000),
            (Kind::DevExp, 0xa26000, 0x10000),
            (Kind::Gbe10_0, 0xa36000, 0x5ba000),
            (Kind::Ptt, 0xff0000, 0x10000),
            (Kind::Bios, 0x1000000, 0x1000000),
        ]);
        let plan = crate::plan::FlashTransport::IntelDescriptor {
            version: IfdVersion::V2,
            capacity: 32 << 20,
            regions,
        };
        let fstart_core::FlashLayout::IntelIfd(layout) = plan.decode().unwrap() else {
            unreachable!()
        };
        let mut rom = vec![0x57; 32 << 20];
        for (at, word) in [
            (0x10, 0x0ff0_a55au32),
            (0x14, 0x0004_0003),
            (0x18, 0x0010_0008),
            (0x30, 0x649c_00f6),
        ] {
            rom[at..at + 4].copy_from_slice(&word.to_le_bytes());
        }
        for (i, word) in [
            0u32, 0x1fff1000, 0x0a250003, 0x00020001, 0x7fff, 0x0a350a26, 0x7fff, 0x7fff, 0x7fff,
            0x7fff, 0x7fff, 0x0fef0a36, 0x7fff, 0x7fff, 0x7fff, 0x0fff0ff0,
        ]
        .into_iter()
        .enumerate()
        {
            rom[0x40 + i * 4..0x44 + i * 4].copy_from_slice(&word.to_le_bytes());
        }
        assert_eq!(validate(&layout, &rom).unwrap(), 16 << 20..32 << 20);
        rom[0x7c..0x80].copy_from_slice(&0x7fffu32.to_le_bytes());
        assert!(
            validate(&layout, &rom)
                .unwrap_err()
                .contains("ptt region mismatch")
        );
    }

    #[test]
    fn wellsburg_short_table_two_components_secondary_bios_and_trailing_gap() {
        let mut rom = vec![0x57; 3 << 20]; // one 1 MiB + one 2 MiB die
        for (at, word) in [
            (0x10, 0x0ff0_a55au32),
            (0x14, 0x0004_0103),
            (0x18, 0x0010_0006),
            (0x30, 0x21),
            (0x40, 0),
            (0x44, 0x00ff0001),
            (0x48, 0xfff),
            (0x4c, 0xfff),
            (0x50, 0xfff),
            (0x54, 0xfff),
            (0x58, 0x01ff0100),
            (0x5c, u32::MAX),
        ] {
            rom[at..at + 4].copy_from_slice(&word.to_le_bytes());
        }
        let plan = crate::plan::FlashTransport::IntelDescriptor {
            version: IfdVersion::V1_5,
            capacity: 3 << 20,
            regions: IfdTransport(vec![
                (Kind::Descriptor, 0, 0x1000),
                (Kind::Bios, 0x1000, 0xff000),
                (Kind::Bios2, 0x100000, 0x100000),
            ]),
        };
        let fstart_core::FlashLayout::IntelIfd(layout) = plan.decode().unwrap() else {
            unreachable!()
        };
        let (full, _) = compose(&layout, &rom, &vec![3; rom.len()]).unwrap();
        assert_eq!(&full[..0x1000], &rom[..0x1000]);
        assert!(full[0x1000..0x100000].iter().all(|b| *b == 3));
        assert_eq!(&full[0x100000..], &rom[0x100000..]);
        assert_eq!(layout.bios_base(), Some(0x1_0000_0000 - 0xff000));
    }

    #[test]
    fn all_sixteen_slots_are_numbered_compared_and_round_trip_in_the_bundle() {
        use crate::plan::FlashTransport;
        let regions = Kind::REGISTERS
            .iter()
            .enumerate()
            .map(|(slot, &kind)| {
                assert_eq!(kind.flreg_index(), Some(slot));
                (kind, slot as u32 * 4096, 4096)
            })
            .collect();
        let plan = FlashTransport::IntelDescriptor {
            version: IfdVersion::V2,
            capacity: 1 << 20,
            regions: IfdTransport(regions),
        };
        let json = serde_json::to_string(&plan).unwrap();
        let fstart_core::FlashLayout::IntelIfd(layout) =
            serde_json::from_str::<FlashTransport>(&json)
                .unwrap()
                .decode()
                .unwrap()
        else {
            unreachable!()
        };
        let mut rom = vec![0x5a; 1 << 20];
        // Reordered sections: FRBA before FCBA; the closest section, not a
        // hard-coded master-table distance, bounds FLREG. NR is reserved.
        for (at, value) in [
            (16, 0x0ff0_a55au32),
            (20, 0x0002_0006),
            (24, 0x0010_0007),
            (0x60, 1),
        ] {
            rom[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        for i in 0..16u32 {
            let at = 0x20 + i as usize * 4;
            rom[at..at + 4].copy_from_slice(&(i | i << 16).to_le_bytes());
        }
        assert_eq!(validate(&layout, &rom).unwrap(), 4096..8192);
        // A changed range in any slot is a mismatch, including reserved slots.
        for i in 0..16 {
            let end = 0x22 + i * 4;
            rom[end] ^= 1;
            assert!(validate(&layout, &rom).is_err());
            rom[end] ^= 1;
        }
    }

    #[test]
    fn refuses_wrong_inputs_not_just_wrong_sizes() {
        let (layout, original) = fixture();
        assert!(validate(&layout, &original[1..]).is_err());
        for (offset, value) in [
            (0x10, 0u32),
            (0x14, 0x0004_0003),
            (0x14, 0x0104_0003),
            (0x14, 0x0303_0003),
            (0x30, 1),
            (0x44, 0x007f_0041),
            (0x48, 0x0003_0003),
            (0x4c, 0x1fff),
        ] {
            let mut rom = original.clone();
            rom[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            assert!(
                validate(&layout, &rom).is_err(),
                "accepted word {offset:x} = {value:x}"
            );
        }
        let mut rom = original;
        rom[4096..12288].fill(0xff);
        assert!(validate(&layout, &rom).is_err());
    }
}
