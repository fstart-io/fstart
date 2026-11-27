//! The APCB v3 container (BIOS directory entries 0x60/0x68 of the SoCs with
//! the `ECB2` header extension, Venice among them) and the records of it
//! fstart edits: the DF group's CXL region descriptors (type `0xCC`) and
//! the token entries the ABL reads its settings from ([`token`], their
//! names in [`names`]).
//!
//! An APCB is a header (`APCB`, its size, a unique instance, a byte
//! checksum over the whole APCB), groups (`DFG `, `MEMG`, ... with a group
//! ID) and in each group typed entries with a 16-byte header. Editing in
//! place keeps every size: the CXL region array is a fixed 48-entry type
//! the board's APCB already carries (MD25-CG0's apcb-60-0 and apcb-68-8,
//! all zero), so writing regions changes no layout.
//!
//! Layout as found in the MD25-CG0 F01 APCBs (offsets checked against the
//! amd-apcb crate's v3 structures); the region descriptor from Venice
//! CxlV2Manager (`ApcbGetType(5, 0x1703, 0xCC)`, `CxlMgrDumpApcbRange`,
//! `CxlMgrBuildApcbEntry` `0x7CC0`, reports/research/CXL.md §2.7,
//! CXL_COMPLETE.md §2.1).
//!
//! An APCB the PSP signs carries an HMAC in the 0x100 bytes after its
//! `apcb_size` ([`HMAC_SIZE`]); the vendors' APCB library treats a tail of
//! one repeated byte as "no HMAC" and falls back to the byte checksum.
//! Every shipped Venice APCB has an all-0xFF tail, so an edit erases the
//! tail ([`commit`]) rather than leave a stale HMAC.

pub mod names;
pub mod token;

use core::ops::Range;

use zerocopy::little_endian::{U16, U32, U64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

/// The header's signature.
pub const SIGNATURE: [u8; 4] = *b"APCB";
/// The container version this module reads (`0x30`).
pub const VERSION: u16 = 0x30;
/// Offset of the checksum byte in the header.
pub const CHECKSUM: usize = 0x10;
/// Offset of the unique-instance dword, which the vendor's APCB library
/// increments on every change.
pub const UNIQUE_INSTANCE: usize = 0x0c;
/// Bytes of the HMAC after `apcb_size` (`ApcbCalcCheckSumExtended`).
pub const HMAC_SIZE: usize = 0x100;

/// Group IDs.
pub mod group {
    pub const PSP: u16 = 0x1701;
    pub const CCX: u16 = 0x1702;
    pub const DF: u16 = 0x1703;
    pub const MEMORY: u16 = 0x1704;
    pub const GNB: u16 = 0x1705;
    pub const FCH: u16 = 0x1706;
    pub const CBS: u16 = 0x1707;
    pub const TOKEN: u16 = 0x3000;
}

/// DF group type of the CXL region descriptor array.
pub const CXL_REGIONS: u16 = 0xcc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Not an APCB v3 or its sizes do not fit the buffer.
    Header,
    /// A group or entry header runs past its container.
    Truncated(usize),
    /// The entry has another size than the records written to it.
    EntrySize { expected: usize, actual: usize },
    /// More records than the entry holds.
    TooMany(usize),
    /// Growing the APCB needs a buffer of this many bytes: the APCB, its
    /// HMAC tail if the buffer holds one, and erased (0xFF) room after.
    NoRoom { needed: usize },
    /// Bytes other than 0xFF after the APCB (and its tail) at this offset,
    /// where growing it would write.
    Trailing(usize),
    /// The token entry at this offset is not sorted by token ID.
    Unsorted(usize),
    /// A token value wider than its entry's kind.
    TokenValue { id: u32, value: u32 },
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct Header {
    signature: [u8; 4],
    header_size: U16,
    version: U16,
    apcb_size: U32,
    unique_instance: U32,
    checksum: u8,
    _reserved: [u8; 3],
    _reserved2: [U32; 3],
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct GroupHeader {
    signature: [u8; 4],
    group: U16,
    header_size: U16,
    version: U16,
    _reserved: U16,
    group_size: U32,
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct EntryHeader {
    group: U16,
    kind: U16,
    entry_size: U16,
    instance: U16,
    context_type: u8,
    context_format: u8,
    unit_size: u8,
    priority_mask: u8,
    key_size: u8,
    key_pos: u8,
    board_mask: U16,
}
const ENTRY_HEADER: usize = size_of::<EntryHeader>();
const _: () = assert!(ENTRY_HEADER == 16 && size_of::<GroupHeader>() == 16);

/// One typed entry of an APCB: its header fields and where its data is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub group: u16,
    pub kind: u16,
    pub instance: u16,
    pub priority_mask: u8,
    pub board_mask: u16,
    /// The entry's data (after its 16-byte header) in the APCB.
    pub data: Range<usize>,
}

/// The validated extent of an APCB v3: its bytes up to `apcb_size`.
fn extent(apcb: &[u8]) -> Result<(usize, usize), Error> {
    let (header, _) = Header::ref_from_prefix(apcb).map_err(|_| Error::Header)?;
    let (start, size) = (
        usize::from(header.header_size.get()),
        header.apcb_size.get() as usize,
    );
    if header.signature != SIGNATURE
        || header.version.get() != VERSION
        || start < size_of::<Header>()
        || size < start
        || size > apcb.len()
    {
        return Err(Error::Header);
    }
    Ok((start, size))
}

/// Walk the entries of `apcb`, group by group, until `stop` takes one:
/// that entry, or `None` after the last.
pub fn walk(apcb: &[u8], mut stop: impl FnMut(&Entry) -> bool) -> Result<Option<Entry>, Error> {
    let (mut group_at, size) = extent(apcb)?;
    while group_at + size_of::<GroupHeader>() <= size {
        let (group, _) = GroupHeader::ref_from_prefix(&apcb[group_at..size])
            .map_err(|_| Error::Truncated(group_at))?;
        let group_end = group_at
            .checked_add(group.group_size.get() as usize)
            .filter(|&end| end <= size && end > group_at)
            .ok_or(Error::Truncated(group_at))?;
        let mut at = group_at + usize::from(group.header_size.get());
        while at + ENTRY_HEADER <= group_end {
            let (entry, _) = EntryHeader::ref_from_prefix(&apcb[at..group_end])
                .map_err(|_| Error::Truncated(at))?;
            let length = usize::from(entry.entry_size.get());
            let end = at
                .checked_add(length)
                .filter(|&end| length >= ENTRY_HEADER && end <= group_end)
                .ok_or(Error::Truncated(at))?;
            let entry = Entry {
                group: entry.group.get(),
                kind: entry.kind.get(),
                instance: entry.instance.get(),
                priority_mask: entry.priority_mask,
                board_mask: entry.board_mask.get(),
                data: at + ENTRY_HEADER..end,
            };
            if stop(&entry) {
                return Ok(Some(entry));
            }
            at = end.next_multiple_of(4);
        }
        group_at = group_end;
    }
    Ok(None)
}

/// Entry `(group, kind, instance)` of `apcb`.
pub fn find(apcb: &[u8], group: u16, kind: u16, instance: u16) -> Result<Option<Entry>, Error> {
    walk(apcb, |e| {
        (e.group, e.kind, e.instance) == (group, kind, instance)
    })
}

/// Make the bytes of `apcb` sum to zero again after an edit.
pub fn fix_checksum(apcb: &mut [u8]) -> Result<(), Error> {
    let (_, size) = extent(apcb)?;
    apcb[CHECKSUM] = 0;
    let sum = apcb[..size].iter().fold(0u8, |a, &b| a.wrapping_add(b));
    apcb[CHECKSUM] = sum.wrapping_neg();
    Ok(())
}

/// `SizeOfApcb` of a checked header: where its HMAC tail starts.
pub fn size(apcb: &[u8]) -> Result<usize, Error> {
    Ok(extent(apcb)?.1)
}

/// Whether the bytes of `apcb` sum to zero.
pub fn checksum_ok(apcb: &[u8]) -> Result<bool, Error> {
    let (_, size) = extent(apcb)?;
    Ok(apcb[..size].iter().fold(0u8, |a, &b| a.wrapping_add(b)) == 0)
}

/// The memory a CXL region holds (descriptor `+0x12`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
pub enum CxlMemory {
    Volatile = 0,
    Persistent = 1,
    /// A type-2 device's ("dual mode", cache).
    Cache = 2,
    /// Volatile memory of a device that is not a memory device, which the
    /// vendor maps as persistent without the specific-purpose attribute.
    OtherPersistent = 3,
}

/// One CXL region the ABL places in the system address map and gives a DF
/// DRAM rule (`CXL_REGION_DESCRIPTOR`, 0x1C bytes): the memory behind the
/// root ports of `nbio_map` on `(socket, die)`. The ABL reports what it
/// mapped in APOB 9/0x18.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "serde",
    serde(rename_all = "kebab-case", deny_unknown_fields)
)]
pub struct CxlRegion {
    /// Bytes; a zero size is an unused entry.
    pub size: u64,
    /// log2 of the window alignment: `0x20` (4 GiB) for volatile and
    /// persistent regions, 0 for a type-2 device's
    /// (`CxlMgrBuildApcbEntry`).
    #[cfg_attr(feature = "serde", serde(default = "four_gib"))]
    pub alignment: u8,
    pub socket: u8,
    pub die: u8,
    /// Bitmap of the PCIe cores of the die the region is behind (bit = the
    /// core of the root port, `CxlMgrNbioIdToBitmap`; not the IOHC).
    pub nbio_map: u8,
    /// Encoded interleave size; `0xFF` without interleave (the vendor's
    /// value for non-type-2 regions).
    #[cfg_attr(feature = "serde", serde(default = "no_interleave"))]
    pub interleave_size: u8,
    #[cfg_attr(feature = "serde", serde(default))]
    pub sub_interleave_map: [u8; 4],
    #[cfg_attr(feature = "serde", serde(default = "no_interleave"))]
    pub sub_interleave_size: u8,
    pub memory: CxlMemory,
    /// CXL device type (1, 2, 3).
    #[cfg_attr(feature = "serde", serde(default = "type3"))]
    pub device_type: u8,
    /// Current link speed and flit status as the vendor DXE records them;
    /// informational for the ABL as far as known.
    #[cfg_attr(feature = "serde", serde(default))]
    pub link_speed: u8,
    #[cfg_attr(feature = "serde", serde(default))]
    pub flit: u8,
    /// The device's PCI address (a restricted CXL host's device bus is the
    /// CFMWS target).
    #[cfg_attr(feature = "serde", serde(default))]
    pub segment: u8,
    #[cfg_attr(feature = "serde", serde(default))]
    pub bus: u8,
    #[cfg_attr(feature = "serde", serde(default))]
    pub device: u8,
    #[cfg_attr(feature = "serde", serde(default))]
    pub function: u8,
    /// The vendor DXE's bookkeeping (`+0x1B`): ErrorFlag[3:0] |
    /// Exclude[4] | FailureCount[6:5]; zero in a fresh region.
    #[cfg_attr(feature = "serde", serde(default))]
    pub status: u8,
}

#[cfg(feature = "serde")]
const fn no_interleave() -> u8 {
    0xff
}
#[cfg(feature = "serde")]
const fn type3() -> u8 {
    3
}
#[cfg(feature = "serde")]
const fn four_gib() -> u8 {
    ALIGN_4GIB
}

/// The alignment of a volatile or persistent region (log2 4 GiB).
pub const ALIGN_4GIB: u8 = 0x20;
/// Bytes of the region array.
pub const CXL_ARRAY_SIZE: usize = MAX_CXL_REGIONS * CXL_REGION_SIZE;

/// Most regions in the array.
pub const MAX_CXL_REGIONS: usize = 48;
/// Bytes of one descriptor.
pub const CXL_REGION_SIZE: usize = 0x1c;

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned, Default)]
#[repr(C)]
struct RawRegion {
    size: U64,
    alignment: u8,
    socket: u8,
    die: u8,
    nbio_map: u8,
    interleave_size: u8,
    sub_interleave_map: [u8; 4],
    sub_interleave_size: u8,
    memory: u8,
    device_type: u8,
    link_speed: u8,
    flit: u8,
    segment: u8,
    bus: u8,
    device: u8,
    function: u8,
    _reserved: u8,
    /// ErrorFlag[3:0] | Exclude[4] | FailureCount[6:5]: the vendor DXE's
    /// bookkeeping, zero in a fresh region.
    status: u8,
}
const _: () = assert!(size_of::<RawRegion>() == CXL_REGION_SIZE);

impl CxlRegion {
    /// The descriptor's bytes.
    pub fn bytes(&self) -> [u8; CXL_REGION_SIZE] {
        let raw = RawRegion {
            size: self.size.into(),
            alignment: self.alignment,
            socket: self.socket,
            die: self.die,
            nbio_map: self.nbio_map,
            interleave_size: self.interleave_size,
            sub_interleave_map: self.sub_interleave_map,
            sub_interleave_size: self.sub_interleave_size,
            memory: self.memory as u8,
            device_type: self.device_type,
            link_speed: self.link_speed,
            flit: self.flit,
            segment: self.segment,
            bus: self.bus,
            device: self.device,
            function: self.function,
            status: self.status,
            ..Default::default()
        };
        let mut out = [0; CXL_REGION_SIZE];
        out.copy_from_slice(raw.as_bytes());
        out
    }

    /// The descriptor in `bytes`; a memory value other than 0..2 reads as
    /// volatile.
    pub fn parse(bytes: &[u8; CXL_REGION_SIZE]) -> Self {
        let raw = RawRegion::ref_from_bytes(bytes).expect("descriptor size");
        Self {
            size: raw.size.get(),
            alignment: raw.alignment,
            socket: raw.socket,
            die: raw.die,
            nbio_map: raw.nbio_map,
            interleave_size: raw.interleave_size,
            sub_interleave_map: raw.sub_interleave_map,
            sub_interleave_size: raw.sub_interleave_size,
            memory: match raw.memory {
                1 => CxlMemory::Persistent,
                2 => CxlMemory::Cache,
                3 => CxlMemory::OtherPersistent,
                _ => CxlMemory::Volatile,
            },
            device_type: raw.device_type,
            link_speed: raw.link_speed,
            flit: raw.flit,
            segment: raw.segment,
            bus: raw.bus,
            device: raw.device,
            function: raw.function,
            status: raw.status,
        }
    }
}

/// The region array of `apcb` (DF type `0xCC`, instance 0), if it has one.
pub fn cxl_array(apcb: &[u8]) -> Result<Option<&[u8; CXL_ARRAY_SIZE]>, Error> {
    let Some(entry) = find(apcb, group::DF, CXL_REGIONS, 0)? else {
        return Ok(None);
    };
    if entry.data.len() != CXL_ARRAY_SIZE {
        return Err(Error::EntrySize {
            expected: CXL_ARRAY_SIZE,
            actual: entry.data.len(),
        });
    }
    Ok(apcb[entry.data].as_array())
}

/// The HMAC tail of `apcb`: the [`HMAC_SIZE`] bytes after its size, if
/// the buffer holds them.
pub fn hmac_tail(apcb: &[u8]) -> Result<Option<&[u8]>, Error> {
    let (_, size) = extent(apcb)?;
    Ok(apcb.get(size..size + HMAC_SIZE))
}

/// Whether a tail is an HMAC: not one repeated byte (the vendors' "not
/// available" test).
pub fn hmac_used(tail: &[u8]) -> bool {
    tail.first().is_some_and(|&b| tail.iter().any(|&x| x != b))
}

/// Seal an edit of `apcb` as the vendors' APCB library does: unique
/// instance + 1 (the ABL's "settings changed"), checksum, and the HMAC
/// tail erased to 0xFF when the buffer holds it (a stale HMAC would not
/// verify; an erased one means "no HMAC").
pub fn commit(apcb: &mut [u8]) -> Result<(), Error> {
    let (_, size) = extent(apcb)?;
    let unique = u32::from_le_bytes(
        apcb[UNIQUE_INSTANCE..UNIQUE_INSTANCE + 4]
            .try_into()
            .map_err(|_| Error::Header)?,
    );
    apcb[UNIQUE_INSTANCE..UNIQUE_INSTANCE + 4]
        .copy_from_slice(&unique.wrapping_add(1).to_le_bytes());
    fix_checksum(apcb)?;
    if let Some(tail) = apcb.get_mut(size..size + HMAC_SIZE) {
        tail.fill(0xff);
    }
    Ok(())
}

/// Write the 0x540-byte region `array` into the CXL region array of
/// `apcb` without sealing the edit ([`commit`]); `Ok(false)` if `apcb`
/// has no such array.
pub fn write_cxl_array(apcb: &mut [u8], array: &[u8; CXL_ARRAY_SIZE]) -> Result<bool, Error> {
    let Some(entry) = find(apcb, group::DF, CXL_REGIONS, 0)? else {
        return Ok(false);
    };
    if entry.data.len() != CXL_ARRAY_SIZE {
        return Err(Error::EntrySize {
            expected: CXL_ARRAY_SIZE,
            actual: entry.data.len(),
        });
    }
    apcb[entry.data].copy_from_slice(array);
    Ok(true)
}

/// Write the 0x540-byte region `array` into `apcb` as [`set_cxl_regions`]
/// does: unique instance + 1, checksum, the HMAC tail erased to 0xFF.
pub fn set_cxl_array(apcb: &mut [u8], array: &[u8; CXL_ARRAY_SIZE]) -> Result<bool, Error> {
    if !write_cxl_array(apcb, array)? {
        return Ok(false);
    }
    commit(apcb)?;
    Ok(true)
}

/// The region array of `regions` (the rest zeroed).
pub fn cxl_array_of(regions: &[CxlRegion]) -> Result<[u8; CXL_ARRAY_SIZE], Error> {
    if regions.len() > MAX_CXL_REGIONS {
        return Err(Error::TooMany(regions.len()));
    }
    let mut array = [0u8; CXL_ARRAY_SIZE];
    for (slot, region) in array
        .as_chunks_mut::<CXL_REGION_SIZE>()
        .0
        .iter_mut()
        .zip(regions)
    {
        slot.copy_from_slice(&region.bytes());
    }
    Ok(array)
}

/// Write `regions` into the CXL region array of `apcb` (DF type `0xCC`,
/// instance 0; the rest of the array zeroed), bump the unique instance,
/// fix the checksum and erase the HMAC tail when the buffer holds it.
/// `Ok(false)` if `apcb` has no such array: the board's APCB must carry it
/// (the array is not created here).
pub fn set_cxl_regions(apcb: &mut [u8], regions: &[CxlRegion]) -> Result<bool, Error> {
    set_cxl_array(apcb, &cxl_array_of(regions)?)
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    /// An APCB shaped like MD25-CG0's apcb-60-0: the 0x80-byte header with
    /// its ECB2 extension, the DF group with the zeroed 0xCC array, and a
    /// token group.
    fn md25_like() -> Vec<u8> {
        let mut apcb = Vec::new();
        let header = |size: u32| {
            let mut h = Vec::new();
            h.extend_from_slice(b"APCB");
            h.extend_from_slice(&0x80u16.to_le_bytes());
            h.extend_from_slice(&0x30u16.to_le_bytes());
            h.extend_from_slice(&size.to_le_bytes());
            h.extend_from_slice(&0x082d_3c5eu32.to_le_bytes());
            h.resize(0x20, 0);
            h.extend_from_slice(b"ECB2");
            h.resize(0x80, 0);
            h
        };
        let df_size = 16 + 16 + 0x540;
        apcb.extend(header(0));
        apcb.extend_from_slice(b"DFG ");
        apcb.extend_from_slice(&0x1703u16.to_le_bytes());
        apcb.extend_from_slice(&16u16.to_le_bytes());
        apcb.extend_from_slice(&1u16.to_le_bytes());
        apcb.extend_from_slice(&0u16.to_le_bytes());
        apcb.extend_from_slice(&(df_size as u32).to_le_bytes());
        // Entry header: group, type, size, instance, ctx, fmt, unit, prio,
        // key size, key pos, board mask (as the donor's 0x90..0xa0).
        apcb.extend_from_slice(&[
            0x03, 0x17, 0xcc, 0x00, 0x50, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00,
            0xff, 0xff,
        ]);
        apcb.resize(0x80 + df_size, 0);
        apcb.extend_from_slice(b"TOKN");
        apcb.extend_from_slice(&0x3000u16.to_le_bytes());
        apcb.extend_from_slice(&16u16.to_le_bytes());
        apcb.extend_from_slice(&1u16.to_le_bytes());
        apcb.extend_from_slice(&0u16.to_le_bytes());
        apcb.extend_from_slice(&16u32.to_le_bytes());
        let size = apcb.len() as u32;
        apcb[8..12].copy_from_slice(&size.to_le_bytes());
        apcb.resize(0x1000, 0xff);
        fix_checksum(&mut apcb).unwrap();
        apcb
    }

    #[test]
    fn finds_the_df_cxl_array_and_writes_regions_in_place() {
        let mut apcb = md25_like();
        assert!(checksum_ok(&apcb).unwrap());
        let mut all = Vec::new();
        walk(&apcb, |e| {
            all.push(e.clone());
            false
        })
        .unwrap();
        assert_eq!(all.len(), 1);
        let entry = &all[0];
        assert_eq!(
            (entry.group, entry.kind, entry.priority_mask),
            (0x1703, 0xcc, 0x10)
        );
        assert_eq!(entry.data, 0xa0..0xa0 + 0x540);

        let region = CxlRegion {
            size: 0x10_0000_0000,
            alignment: 0,
            socket: 0,
            die: 1,
            nbio_map: 0b10,
            interleave_size: 0xff,
            sub_interleave_map: [0; 4],
            sub_interleave_size: 0xff,
            memory: CxlMemory::Volatile,
            device_type: 3,
            link_speed: 0,
            flit: 0,
            segment: 0,
            bus: 0,
            device: 0,
            function: 0,
            status: 0x60,
        };
        let before = apcb.clone();
        assert!(set_cxl_regions(&mut apcb, &[region]).unwrap());
        assert!(checksum_ok(&apcb).unwrap());
        assert_eq!(apcb.len(), before.len());
        assert_eq!(&apcb[0xa0..0xa8], &0x10_0000_0000u64.to_le_bytes());
        assert_eq!(&apcb[0xa8..0xac], &[0, 0, 1, 0b10]);
        assert_eq!(apcb[0xac], 0xff);
        assert_eq!(apcb[0xa0 + 0x11], 0xff);
        assert_eq!(apcb[0xa0 + 0x13], 3);
        assert_eq!(apcb[0xa0 + 0x1b], 0x60);
        assert!(apcb[0xa0 + 0x1c..0xa0 + 0x540].iter().all(|&b| b == 0));
        let array = cxl_array(&apcb).unwrap().unwrap();
        assert_eq!(CxlRegion::parse(array[..0x1c].try_into().unwrap()), region);
        assert_eq!(&apcb[0x0c..0x10], &0x082d_3c5fu32.to_le_bytes());
        // Only the array, the unique instance and the checksum changed (the
        // tail was erased already).
        let changed: Vec<usize> = (0..apcb.len()).filter(|&i| apcb[i] != before[i]).collect();
        assert!(changed.iter().all(|&i| (0xa0..0xa0 + 0x1c).contains(&i)
            || (0x0c..0x10).contains(&i)
            || i == CHECKSUM));

        // A signed APCB loses its HMAC (a stale one would not verify).
        let size = u32::from_le_bytes(apcb[8..12].try_into().unwrap()) as usize;
        apcb[size..size + HMAC_SIZE].fill(0x5a);
        apcb[size + 3] = 0x11;
        assert!(hmac_used(hmac_tail(&apcb).unwrap().unwrap()));
        assert!(set_cxl_regions(&mut apcb, &[]).unwrap());
        assert!(!hmac_used(hmac_tail(&apcb).unwrap().unwrap()));
        assert!(apcb[size..size + HMAC_SIZE].iter().all(|&b| b == 0xff));
        assert!(checksum_ok(&apcb).unwrap());

        // An APCB without the array is left alone.
        let mut other = md25_like();
        other[0x90 + 2] = 0xcd;
        fix_checksum(&mut other).unwrap();
        assert!(!set_cxl_regions(&mut other, &[region]).unwrap());
        assert_eq!(
            set_cxl_regions(&mut apcb, &[region; 49]),
            Err(Error::TooMany(49))
        );
        assert_eq!(find(b"APCB", 0, 0, 0), Err(Error::Header));
    }
}
