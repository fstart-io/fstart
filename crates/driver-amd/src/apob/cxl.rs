// SPDX-License-Identifier: MIT OR Apache-2.0
//! APOB `9/0x18` (`CXL_ADDR_MAP_INFO_V2`): the CXL windows the ABL placed in
//! the system address map from the APCB's CXL region descriptors and gave
//! DF DRAM rules. x86 never programs DF rules for CXL; it reads them here.
//!
//! Entry: `+0x40` u32 offset of the window array from the entry's start,
//! `+0x48` u32 count; windows of 0x40 bytes, the first with size 0 ending
//! the list (SilVenicePei `ApobV2GetCxlDevice` `0xF568A0`, instruction-
//! checked: `[entry + [entry+0x40] + 0x40n]`, `n < [entry+0x48]`, stop on
//! `size == 0`). Window fields from CxlV2Manager `CxlMgrDumpApobRange`
//! (reports/research/CXL.md §2.6). Turin uses the same group and type.

use heapless::Vec;
use zerocopy::little_endian::{U16, U32, U64};
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

use super::ApobError;
use super::v2::{ApobV2, EntryV2, group};

/// APOB V2 type of the CXL address map in the fabric group.
pub const KIND: u32 = 0x18;
/// Most windows (CxlV2Manager rejects a count above 0x30).
pub const MAX_WINDOWS: usize = 0x30;
/// Most interleave targets of one window.
pub const MAX_TARGETS: usize = 0x20;
/// Window status: the ABL could not fit the region into the memory map.
pub const STATUS_OUT_OF_MEMORY_MAP: u16 = 1;

/// The memory of a window (`+0x1E`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Memory {
    Volatile,
    Persistent,
    /// A type-2 device's ("dual mode").
    DeviceCoherent,
    /// Another persistent kind (the vendor converts it to persistent memory
    /// without the specific-purpose attribute).
    OtherPersistent,
    Unknown(u8),
}

impl Memory {
    const fn of(raw: u8) -> Self {
        match raw {
            0 => Self::Volatile,
            1 => Self::Persistent,
            2 => Self::DeviceCoherent,
            3 => Self::OtherPersistent,
            other => Self::Unknown(other),
        }
    }
}

/// One CXL window of APOB `9/0x18`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    pub base: u64,
    pub size: u64,
    pub status: u16,
    pub socket: u8,
    /// Bitmap of the IOHCs on the socket the window's devices are behind.
    pub nbio_map: u8,
    /// log2 of the alignment.
    pub alignment: u8,
    /// Encoded interleave granularity (`0xFF`: none).
    pub interleave_size: u8,
    pub sub_interleave_map: [u8; 4],
    pub sub_interleave_size: u8,
    pub memory: Memory,
    /// Indices of the interleaved regions in the APCB's CXL region array,
    /// way 0 first.
    pub targets: Vec<u8, MAX_TARGETS>,
}

impl Window {
    /// The ABL mapped the window: it has a size and fit into the memory map.
    pub fn mapped(&self) -> bool {
        self.size != 0 && self.status != STATUS_OUT_OF_MEMORY_MAP
    }

    pub fn end(&self) -> u64 {
        self.base.saturating_add(self.size)
    }
}

#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct RawWindow {
    base: U64,
    size: U64,
    status: U16,
    _reserved: U16,
    socket: u8,
    _reserved2: u8,
    nbio_map: u8,
    alignment: u8,
    interleave_size: u8,
    sub_interleave_map: [u8; 4],
    sub_interleave_size: u8,
    memory: u8,
    target_count: u8,
    targets: [u8; MAX_TARGETS],
}
const _: () = assert!(size_of::<RawWindow>() == 0x40);

/// Decode the windows of an APOB `9/0x18` entry.
pub fn windows(entry: &EntryV2<'_>) -> Result<Vec<Window, MAX_WINDOWS>, ApobError> {
    let value = |value: u32| ApobError::Value {
        group: group::FABRIC,
        kind: KIND,
        value,
    };
    let offset = entry.at::<U32>(0x40)?.get();
    let count = entry.at::<U32>(0x48)?.get();
    if count as usize > MAX_WINDOWS {
        return Err(value(count));
    }
    if offset < 0x4c {
        return Err(value(offset));
    }
    let raw = entry.slice::<RawWindow>(offset as usize, count as usize)?;
    let mut out = Vec::new();
    for w in raw.iter().take_while(|w| w.size.get() != 0) {
        let targets = usize::from(w.target_count);
        if targets > MAX_TARGETS {
            return Err(value(w.target_count.into()));
        }
        let window = Window {
            base: w.base.get(),
            size: w.size.get(),
            status: w.status.get(),
            socket: w.socket,
            nbio_map: w.nbio_map,
            alignment: w.alignment,
            interleave_size: w.interleave_size,
            sub_interleave_map: w.sub_interleave_map,
            sub_interleave_size: w.sub_interleave_size,
            memory: Memory::of(w.memory),
            targets: Vec::from_slice(&w.targets[..targets]).map_err(|_| value(targets as u32))?,
        };
        if window.base.checked_add(window.size).is_none() {
            return Err(value(0));
        }
        let _ = out.push(window);
    }
    Ok(out)
}

impl ApobV2<'_> {
    /// The CXL windows of APOB `9/0x18`; none when the ABL wrote no record
    /// (no CXL regions in the APCB).
    pub fn cxl_windows(&self) -> Result<Vec<Window, MAX_WINDOWS>, ApobError> {
        match self.find(group::FABRIC, KIND, 0) {
            Ok(entry) => windows(&entry),
            Err(ApobError::NotFound { .. }) => Ok(Vec::new()),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec as StdVec;

    /// An APOB V2 with one section holding a `9/0x18` entry with `windows`.
    pub(crate) fn apob(windows: &[[u8; 0x40]]) -> StdVec<u8> {
        let mut entry = StdVec::new();
        let size = 0x50 + 0x40 * windows.len();
        for word in [group::FABRIC, KIND, 0, size as u32] {
            entry.extend_from_slice(&word.to_le_bytes());
        }
        entry.resize(0x40, 0);
        entry.extend_from_slice(&0x50u32.to_le_bytes());
        entry.extend_from_slice(&0u32.to_le_bytes());
        entry.extend_from_slice(&(windows.len() as u32).to_le_bytes());
        entry.resize(0x50, 0);
        windows.iter().for_each(|w| entry.extend_from_slice(w));
        let section_len = 0x28 + entry.len();
        let mut blob = StdVec::new();
        blob.extend_from_slice(b"APOB");
        for word in [0x20u32, 1, 1, 0, 0] {
            blob.extend_from_slice(&word.to_le_bytes());
        }
        blob.extend_from_slice(&0x1cu32.to_le_bytes());
        blob.extend_from_slice(&(section_len as u32).to_le_bytes());
        blob.resize(0x1c + 0x28, 0);
        blob.extend_from_slice(&entry);
        let total = blob.len() as u32;
        blob[0x10..0x14].copy_from_slice(&total.to_le_bytes());
        blob
    }

    pub(crate) fn window(
        base: u64,
        size: u64,
        socket: u8,
        nbio_map: u8,
        targets: &[u8],
    ) -> [u8; 0x40] {
        let mut w = [0u8; 0x40];
        w[0..8].copy_from_slice(&base.to_le_bytes());
        w[8..16].copy_from_slice(&size.to_le_bytes());
        w[0x14] = socket;
        w[0x16] = nbio_map;
        w[0x17] = 28;
        w[0x18] = 0xff;
        w[0x1d] = 0xff;
        w[0x1f] = targets.len() as u8;
        w[0x20..0x20 + targets.len()].copy_from_slice(targets);
        w
    }

    #[test]
    fn decodes_windows_up_to_the_first_empty_one() {
        let mut out_of_map = window(0x500_0000_0000, 0x10_0000_0000, 1, 2, &[1]);
        out_of_map[0x10] = 1;
        out_of_map[0x1e] = 1;
        let blob = apob(&[
            window(0x400_0000_0000, 0x40_0000_0000, 0, 1, &[0, 2]),
            out_of_map,
            [0; 0x40],
            window(0x600_0000_0000, 0x10_0000_0000, 0, 1, &[3]),
        ]);
        let apob = ApobV2::parse(&blob).unwrap();
        let windows = apob.cxl_windows().unwrap();
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].base, 0x400_0000_0000);
        assert_eq!(windows[0].targets[..], [0, 2]);
        assert_eq!(windows[0].memory, Memory::Volatile);
        assert!(windows[0].mapped());
        assert_eq!(windows[1].socket, 1);
        assert_eq!(windows[1].memory, Memory::Persistent);
        assert!(!windows[1].mapped());
    }

    #[test]
    fn no_record_is_no_windows_and_a_bad_count_is_an_error() {
        let blob = apob(&[]);
        let apob_v2 = ApobV2::parse(&blob).unwrap();
        assert!(apob_v2.cxl_windows().unwrap().is_empty());
        let mut bad = apob(&[window(0, 0x1000_0000, 0, 1, &[0])]);
        let count_at = 0x1c + 0x28 + 0x48;
        bad[count_at..count_at + 4].copy_from_slice(&0x31u32.to_le_bytes());
        let apob_v2 = ApobV2::parse(&bad).unwrap();
        assert!(apob_v2.cxl_windows().is_err());
    }
}
