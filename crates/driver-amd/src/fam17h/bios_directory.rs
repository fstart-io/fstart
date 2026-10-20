// SPDX-License-Identifier: MIT OR Apache-2.0
//! Read-only view of the BIOS directory in the boot flash, for files the
//! vendor image keeps there, e.g. microcode (type 0x66). When the first level
//! (`$BHD`) points to a second level (`$BL2`, type 0x70), the PSP uses the
//! second level and so does this.
use zerocopy::{
    FromBytes, Immutable, KnownLayout, Unaligned,
    byteorder::{LittleEndian, U32, U64},
};

const COOKIE: &[u8; 4] = b"$BHD";
const LEVEL2_COOKIE: &[u8; 4] = b"$BL2";
const LEVEL2: u8 = 0x70;
/// The vendor images put the directory on a 4-KiB boundary.
const ALIGN: usize = 0x1000;
const MAX_ENTRIES: usize = 64;

#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct Header {
    cookie: [u8; 4],
    checksum: U32<LittleEndian>,
    count: U32<LittleEndian>,
    reserved: U32<LittleEndian>,
}
#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct WireEntry {
    kind: u8,
    region: u8,
    flags: u8,
    subprogram: u8,
    size: U32<LittleEndian>,
    source: U64<LittleEndian>,
    destination: U64<LittleEndian>,
}

/// A directory entry whose source lies in `flash`.
#[derive(Debug, Clone, Copy)]
pub struct Entry<'a> {
    pub kind: u8,
    pub subprogram: u8,
    pub destination: u64,
    pub data: &'a [u8],
}

fn directory<'a>(flash: &'a [u8], offset: usize, cookie: &[u8; 4]) -> Option<&'a [WireEntry]> {
    let bytes = flash.get(offset..)?;
    let (header, rest) = Header::ref_from_prefix(bytes).ok()?;
    if &header.cookie != cookie {
        return None;
    }
    let count = (header.count.get() as usize).min(MAX_ENTRIES);
    <[WireEntry]>::ref_from_prefix_with_elems(rest, count)
        .ok()
        .map(|(entries, _)| entries)
}

/// Flash offset of an entry's source, which is either an offset or a mapped
/// address below 4 GiB.
fn offset(flash: &[u8], source: u64) -> Option<usize> {
    let flash_base = (1u64 << 32).wrapping_sub(flash.len() as u64);
    usize::try_from(source.checked_sub(flash_base).unwrap_or(source)).ok()
}

/// The entries of the BIOS directory in `flash`, the whole flash image mapped
/// so that it ends at 4 GiB. Entries without contents in the image (e.g. the
/// APOB, which only has a destination) are skipped.
pub fn entries(flash: &[u8]) -> impl Iterator<Item = Entry<'_>> {
    let level1 = (0..flash.len())
        .step_by(ALIGN)
        .find_map(|o| directory(flash, o, COOKIE))
        .unwrap_or(&[]);
    let table = level1
        .iter()
        .find(|e| e.kind == LEVEL2)
        .and_then(|e| directory(flash, offset(flash, e.source.get())?, LEVEL2_COOKIE))
        .unwrap_or(level1);
    table.iter().filter(|e| e.size.get() != 0).filter_map(move |e| {
        let start = offset(flash, e.source.get())?;
        let data = flash.get(start..start.checked_add(e.size.get() as usize)?)?;
        Some(Entry {
            kind: e.kind,
            subprogram: e.subprogram,
            destination: e.destination.get(),
            data,
        })
    })
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    #[test]
    fn finds_microcode_entries_by_mapped_address() {
        let mut flash = std::vec![0xffu8; 0x40000];
        let dir = 0x3000;
        flash[dir..dir + 4].copy_from_slice(COOKIE);
        flash[dir + 8..dir + 12].copy_from_slice(&2u32.to_le_bytes());
        let mut entry = |i: usize, kind: u8, size: u32, source: u64| {
            let e = dir + 16 + i * 24;
            flash[e..e + 24].fill(0);
            flash[e] = kind;
            flash[e + 4..e + 8].copy_from_slice(&size.to_le_bytes());
            flash[e + 8..e + 16].copy_from_slice(&source.to_le_bytes());
        };
        entry(0, 0x61, 0, 0);
        entry(1, 0x66, 0x10, 0xfffc_8000);
        let found: std::vec::Vec<_> = entries(&flash).collect();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, 0x66);
        assert_eq!(found[0].data.as_ptr(), flash[0x8000..].as_ptr());
    }
}
