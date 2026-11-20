//! Import a raw legacy Intel descriptor-mode flash dump. The board plan is
//! the expected geometry, not a substitute for parsing the donor's descriptor.
use crate::bundle::sha256;
use fstart_core::{IntelIfdFlashLayout, IntelIfdRegion as Kind};
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

/// Compare every hardware FLREG (descriptor, BIOS, ME, GbE, PDR) with the
/// board layout. Refuse newer descriptor formats rather than guessing where
/// additional regions or a capsule's SPI payload start.
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
    let nr = (map >> 24) & 7;
    if nc > 1 || !(1..=4).contains(&nr) {
        return Err(
            "unsupported Intel descriptor (expected legacy FLMAP0 with 2–5 regions)".into(),
        );
    }
    let fcba = ((map & 0xff) << 4) as usize;
    let frba = (((map >> 16) & 0xff) << 4) as usize;
    let fmba = ((word(header + 8)? & 0xff) << 4) as usize;
    if fcba < header + 16 || fcba + 12 > frba || frba + 20 > fmba || fmba >= fd.len() {
        return Err("unsupported or overlapping Intel descriptor sections".into());
    }
    let component = word(fcba)?;
    let mut capacity = 0usize;
    for chip in 0..=nc {
        capacity += (512 << 10) << ((component >> (chip * 3)) & 7);
    }
    if capacity != rom.len() {
        return Err(format!(
            "IFD component capacity {capacity:#x} does not match the flash image"
        ));
    }
    for kind in [Kind::Descriptor, Kind::Bios, Kind::Me, Kind::Gbe, Kind::Pdr] {
        let index = kind.flreg_index().unwrap();
        let reg = word(frba + index * 4)?;
        let first = reg & 0xfff;
        let last = (reg >> 16) & 0xfff;
        // Ignore reserved encoding bits only for disabled entries. Never
        // truncate a larger active region to manufacture a layout match.
        let actual = if first > last {
            None
        } else {
            if index as u32 > nr || reg & 0xf000_f000 != 0 {
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
            (0x50, 0x1fff),
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
