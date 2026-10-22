// SPDX-License-Identifier: MIT OR Apache-2.0
//! Directory wire layouts and checked traversal (AMD #55758). Adapted from
//! the independently sourced SP3 reader; packing policy lives in its caller.
use crate::{Error, Flash};
use core::ops::Range;
use tock_registers::{LocalRegisterCopy, register_bitfields};
use zerocopy::little_endian::{U32, U64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

pub const PSP_COOKIE: [u8; 4] = *b"$PSP";
pub const PSP_LEVEL2_COOKIE: [u8; 4] = *b"$PL2";
pub const BIOS_COOKIE: [u8; 4] = *b"$BHD";
pub const BIOS_LEVEL2_COOKIE: [u8; 4] = *b"$BL2";
pub mod psp {
    pub const NVRAM: u8 = 0x04;
    pub const SOFT_FUSE: u8 = 0x0b;
    pub const SEV_DATA: u8 = 0x38;
    pub const LEVEL2: u8 = 0x40;
    pub const RECOVERY_A: u8 = 0x48;
    pub const BIOS_TABLE: u8 = 0x49;
    pub const RECOVERY_B: u8 = 0x4a;
    pub const VERSTAGE: u8 = 0x52;
    pub const VERSTAGE_SIGNATURE: u8 = 0x53;
    pub const RPMC_NVRAM: u8 = 0x54;
}
pub mod bios {
    pub const RTM_PUBKEY: u8 = 0x05;
    pub const SIGNATURE: u8 = 0x07;
    pub const APCB: u8 = 0x60;
    pub const APOB: u8 = 0x61;
    pub const RESET_IMAGE: u8 = 0x62;
    pub const APOB_NV: u8 = 0x63;
    pub const PMU_INSTRUCTIONS: u8 = 0x64;
    pub const PMU_DATA: u8 = 0x65;
    pub const MICROCODE: u8 = 0x66;
    pub const APCB_BACKUP: u8 = 0x68;
    pub const MP2_CONFIG: u8 = 0x6a;
    pub const SHARED_MEMORY: u8 = 0x6b;
    pub const LEVEL2: u8 = 0x70;
}
register_bitfields![u32,
    Info0 [ BLOCKS OFFSET(0) NUMBITS(10) [], MODE OFFSET(29) NUMBITS(2) [], VERSION OFFSET(31) NUMBITS(1) [] ],
    Info1 [ BLOCKS OFFSET(0) NUMBITS(16) [], HEADER_KIB OFFSET(20) NUMBITS(4) [], MODE OFFSET(24) NUMBITS(2) [] ]
];
#[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct Header {
    pub cookie: [u8; 4],
    pub checksum: U32,
    pub count: U32,
    pub additional: U32,
}
impl Header {
    pub fn version(&self) -> u8 {
        (self.additional.get() >> 31) as u8
    }
    pub fn address_mode(&self) -> u8 {
        if self.version() == 0 {
            LocalRegisterCopy::<u32, Info0::Register>::new(self.additional.get()).read(Info0::MODE)
                as u8
        } else {
            LocalRegisterCopy::<u32, Info1::Register>::new(self.additional.get()).read(Info1::MODE)
                as u8
        }
    }
    pub fn directory_size(&self) -> usize {
        let blocks = if self.version() == 0 {
            LocalRegisterCopy::<u32, Info0::Register>::new(self.additional.get())
                .read(Info0::BLOCKS)
        } else {
            LocalRegisterCopy::<u32, Info1::Register>::new(self.additional.get())
                .read(Info1::BLOCKS)
        };
        blocks as usize * 4096
    }
}
#[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct PspEntry {
    pub kind: u8,
    pub subprogram: u8,
    pub attributes: [u8; 2],
    pub size: U32,
    pub address: U64,
}
impl PspEntry {
    pub fn new(kind: u8, subprogram: u8, instance: u8, size: u32, address: u64) -> Self {
        Self {
            kind,
            subprogram,
            attributes: [instance << 3, 0],
            size: size.into(),
            address: address.into(),
        }
    }
    /// The SPI ROM the PSP reads the file from.
    pub fn rom_id(&self) -> u8 {
        self.attributes[0] & 3
    }
    pub fn writable(&self) -> bool {
        self.attributes[0] & 4 != 0
    }
}
#[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct BiosEntry {
    pub kind: u8,
    pub region: u8,
    pub flags: u8,
    pub subprogram: u8,
    pub size: U32,
    pub source: U64,
    pub destination: U64,
}
impl BiosEntry {
    pub const RESET: u8 = 1;
    pub const COPY: u8 = 2;
    pub const READ_ONLY: u8 = 4;
    pub const COMPRESSED: u8 = 8;
    pub fn new(
        kind: u8,
        subprogram: u8,
        instance: u8,
        flags: u8,
        size: u32,
        source: u64,
        destination: u64,
    ) -> Self {
        Self {
            kind,
            region: 0,
            flags: flags | instance << 4,
            subprogram,
            size: size.into(),
            source: source.into(),
            destination: destination.into(),
        }
    }
    pub fn reset(&self) -> bool {
        self.flags & Self::RESET != 0
    }
    pub fn copy(&self) -> bool {
        self.flags & Self::COPY != 0
    }
    pub fn read_only(&self) -> bool {
        self.flags & Self::READ_ONLY != 0
    }
    pub fn compressed(&self) -> bool {
        self.flags & Self::COMPRESSED != 0
    }
    /// The SPI ROM the PSP reads the file from.
    pub fn rom_id(&self) -> u8 {
        self.subprogram >> 3 & 3
    }
    /// Where the PSP copies the file, `u64::MAX` for nowhere; the APOB
    /// entry has only this.
    pub fn destination(&self) -> u64 {
        self.destination.get()
    }
}
const _: () = {
    assert!(size_of::<Header>() == 16);
    assert!(size_of::<PspEntry>() == 16);
    assert!(size_of::<BiosEntry>() == 24);
};
pub trait Entry: FromBytes + KnownLayout + Immutable + Unaligned {
    const COOKIES: [[u8; 4]; 2];
    const LEVEL2: u8;
    fn kind(&self) -> u8;
    fn size(&self) -> u32;
    fn source(&self) -> u64;
    fn instance(&self) -> u8;
    fn subprogram(&self) -> u8;
    fn has_data(&self) -> bool {
        !matches!(self.size(), 0 | u32::MAX) && !matches!(self.source(), 0 | u64::MAX)
    }
    fn apob_nv(&self) -> bool {
        false
    }
}
impl Entry for PspEntry {
    const COOKIES: [[u8; 4]; 2] = [PSP_COOKIE, PSP_LEVEL2_COOKIE];
    const LEVEL2: u8 = psp::LEVEL2;
    fn kind(&self) -> u8 {
        self.kind
    }
    fn size(&self) -> u32 {
        self.size.get()
    }
    fn source(&self) -> u64 {
        self.address.get()
    }
    fn instance(&self) -> u8 {
        (self.attributes[0] >> 3) & 15
    }
    fn subprogram(&self) -> u8 {
        self.subprogram
    }
    fn has_data(&self) -> bool {
        self.kind != psp::SOFT_FUSE && !matches!(self.size(), 0 | u32::MAX)
    }
}
impl Entry for BiosEntry {
    const COOKIES: [[u8; 4]; 2] = [BIOS_COOKIE, BIOS_LEVEL2_COOKIE];
    const LEVEL2: u8 = bios::LEVEL2;
    fn kind(&self) -> u8 {
        self.kind
    }
    fn size(&self) -> u32 {
        self.size.get()
    }
    fn source(&self) -> u64 {
        self.source.get()
    }
    fn instance(&self) -> u8 {
        self.flags >> 4
    }
    fn subprogram(&self) -> u8 {
        self.subprogram & 7
    }
    fn apob_nv(&self) -> bool {
        self.kind == bios::APOB_NV
    }
}

pub struct Directory<'a, E> {
    pub offset: usize,
    pub header: &'a Header,
    pub entries: &'a [E],
    table: &'a [u8],
    flash: Flash<'a>,
}
impl<E> Copy for Directory<'_, E> {}
impl<E> Clone for Directory<'_, E> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<'a, E: Entry> Directory<'a, E> {
    pub fn read(flash: Flash<'a>, offset: usize, level: usize) -> Option<Self> {
        Self::read_checked(flash, offset, level).ok()
    }
    pub fn read_checked(flash: Flash<'a>, offset: usize, level: usize) -> Result<Self, Error> {
        let bytes = flash
            .bytes()
            .get(offset..)
            .ok_or(Error::Truncated(offset))?;
        let (header, rest) =
            Header::ref_from_prefix(bytes).map_err(|_| Error::Truncated(offset))?;
        if E::COOKIES.get(level) != Some(&header.cookie) {
            return Err(Error::Cookie(offset));
        }
        // v1 still stores entries immediately after the 16-byte fixed header;
        // HEADER_KIB describes the header allocation, not an entries offset.
        let (entries, _) = <[E]>::ref_from_prefix_with_elems(rest, header.count.get() as usize)
            .map_err(|_| Error::Truncated(offset))?;
        let table = &bytes[..size_of::<Header>() + size_of_val(entries)];
        if fletcher32(&table[8..]) != header.checksum.get() {
            return Err(Error::Checksum(offset));
        }
        let directory = Self {
            offset,
            header,
            entries,
            table,
            flash,
        };
        directory.extent()?;
        Ok(directory)
    }
    pub fn table(&self) -> &'a [u8] {
        self.table
    }
    /// Allocation extent, including files/padding described by dir_size.
    pub fn extent(&self) -> Result<Range<usize>, Error> {
        let end = self
            .offset
            .checked_add(self.header.directory_size().max(self.table.len()))
            .filter(|&e| e <= self.flash.bytes().len())
            .ok_or(Error::Truncated(self.offset))?;
        Ok(self.offset..end)
    }
    pub fn range(&self, entry: &E) -> Result<Range<usize>, Error> {
        let source = entry.source();
        let start = if entry.apob_nv()
            && self.flash.profile.apob_nv_alias
            && (0xff00_0000..1u64 << 32).contains(&source)
        {
            self.flash
                .origin
                .checked_add((source - 0xff00_0000) as usize)
                .ok_or(Error::Address(source))?
        } else {
            self.flash
                .resolve(source, self.header.address_mode(), self.offset)?
        };
        let end = start
            .checked_add(entry.size() as usize)
            .filter(|&e| e <= self.flash.bytes().len())
            .ok_or(Error::Address(source))?;
        Ok(start..end)
    }
    pub fn data(&self, entry: &E) -> Result<&'a [u8], Error> {
        Ok(&self.flash.bytes()[self.range(entry)?])
    }
    pub fn level2_checked(&self) -> Result<Option<Self>, Error> {
        let mut entries = self.entries.iter().filter(|e| e.kind() == E::LEVEL2);
        let Some(entry) = entries.next() else {
            return Ok(None);
        };
        if entries.next().is_some() {
            return Err(Error::DuplicateLevel2);
        }
        let range = self.range(entry)?;
        let offset = range.start;
        let level2 = Self::read_checked(self.flash, offset, 1)?;
        if offset + level2.table.len() > range.end {
            return Err(Error::Truncated(offset));
        }
        if level2.entries.iter().any(|e| e.kind() == E::LEVEL2) {
            return Err(Error::NestedLevel2);
        }
        Ok(Some(level2))
    }
    pub fn files(&self, _flash: Flash<'a>) -> impl Iterator<Item = (&'a E, &'a [u8])> + use<'a, E> {
        let dir = *self;
        self.entries
            .iter()
            .filter(|e| e.has_data())
            .filter_map(move |entry| Some((entry, dir.data(entry).ok()?)))
    }
}
pub struct Levels<'a, E> {
    pub level1: Directory<'a, E>,
    pub level2: Option<Directory<'a, E>>,
}
impl<'a, E: Entry> Levels<'a, E> {
    pub fn read(flash: Flash<'a>, offset: usize) -> Option<Self> {
        Self::read_checked(flash, offset).ok()
    }
    pub fn read_checked(flash: Flash<'a>, offset: usize) -> Result<Self, Error> {
        let level1 = Directory::read_checked(flash, offset, 0)?;
        let level2 = level1.level2_checked()?;
        Ok(Self { level1, level2 })
    }
    pub fn boot(&self) -> Directory<'a, E> {
        self.level2.unwrap_or(self.level1)
    }
    pub fn iter(&self) -> impl Iterator<Item = Directory<'a, E>> + use<'a, E> {
        core::iter::once(self.level1).chain(self.level2)
    }
    /// L2 first, then L1: the order the PSP and AGESA look files up in.
    fn lookup_order(&self) -> impl Iterator<Item = Directory<'a, E>> + use<'a, E> {
        self.level2.into_iter().chain(core::iter::once(self.level1))
    }
    /// The first entry of `kind` matching `instance` and `subprogram` when
    /// given, L2 first, with its directory.
    pub fn find(
        &self,
        kind: u8,
        instance: Option<u8>,
        subprogram: Option<u8>,
    ) -> Option<(Directory<'a, E>, &'a E)> {
        self.lookup_order().find_map(|dir| {
            dir.entries
                .iter()
                .find(|e| {
                    e.kind() == kind
                        && instance.is_none_or(|i| e.instance() == i)
                        && subprogram.is_none_or(|s| e.subprogram() == s)
                })
                .map(|e| (dir, e))
        })
    }
    /// Every entry of `kind` of the first level that has one, L2 first
    /// (e.g. all microcode instances).
    pub fn all(&self, kind: u8) -> impl Iterator<Item = (Directory<'a, E>, &'a E)> + use<'a, E> {
        self.lookup_order()
            .find(|d| d.entries.iter().any(|e| e.kind() == kind))
            .into_iter()
            .flat_map(move |d| {
                d.entries
                    .iter()
                    .filter(move |e| e.kind() == kind)
                    .map(move |e| (d, e))
            })
    }
}

pub const PSP_COMBO_COOKIE: [u8; 4] = *b"2PSP";
#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct ComboHeader {
    cookie: [u8; 4],
    checksum: U32,
    count: U32,
    mode: U32,
    reserved: [u8; 16],
}
#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct RawComboEntry {
    select: U32,
    id: U32,
    directory: U64,
}
pub struct Combo<'a> {
    offset: usize,
    header: &'a ComboHeader,
    entries: &'a [RawComboEntry],
}
pub struct ComboEntry {
    pub id_select: u32,
    pub id: u32,
    pub directory: u64,
}
impl<'a> Combo<'a> {
    pub fn read(flash: Flash<'a>, offset: usize) -> Result<Option<Self>, Error> {
        let bytes = flash
            .bytes()
            .get(offset..)
            .ok_or(Error::Truncated(offset))?;
        if bytes.get(..4) != Some(&PSP_COMBO_COOKIE) {
            return Ok(None);
        }
        let (header, rest) =
            ComboHeader::ref_from_prefix(bytes).map_err(|_| Error::Truncated(offset))?;
        if header.mode.get() > 1 {
            return Err(Error::ComboSelection);
        }
        let (entries, _) =
            <[RawComboEntry]>::ref_from_prefix_with_elems(rest, header.count.get() as usize)
                .map_err(|_| Error::Truncated(offset))?;
        let end = size_of::<ComboHeader>() + size_of_val(entries);
        if fletcher32(&bytes[8..end]) != header.checksum.get() {
            return Err(Error::Checksum(offset));
        }
        Ok(Some(Self {
            offset,
            header,
            entries,
        }))
    }
    pub fn extent(&self) -> Range<usize> {
        self.offset..self.offset + size_of::<ComboHeader>() + size_of_val(self.entries)
    }
    pub fn lookup_mode(&self) -> u32 {
        self.header.mode.get()
    }
    pub fn entries(&self) -> impl Iterator<Item = ComboEntry> + '_ {
        self.entries.iter().map(|e| ComboEntry {
            id_select: e.select.get(),
            id: e.id.get(),
            directory: e.directory.get(),
        })
    }
}

/// Change a checked table, then recompute its checksum. Callers retain
/// responsibility for the semantic validity of their modified entries.
pub fn modify_entries<E: Entry + IntoBytes>(
    image: &mut [u8],
    offset: usize,
    mut change: impl FnMut(&mut E),
) -> Option<()> {
    let len = Directory::<E>::read_checked(Flash::new(image), offset, 0)
        .or_else(|_| Directory::<E>::read_checked(Flash::new(image), offset, 1))
        .ok()?
        .table()
        .len();
    let bytes = image.get_mut(offset..offset + len)?;
    let (header, rest) = Header::mut_from_prefix(bytes).ok()?;
    let (entries, _) = <[E]>::mut_from_prefix_with_elems(rest, header.count.get() as usize).ok()?;
    entries.iter_mut().for_each(&mut change);
    let sum = fletcher32(&bytes[8..]);
    Header::mut_from_prefix(bytes).ok()?.0.checksum.set(sum);
    Some(())
}

/// Folded Fletcher-32 from the SP3 reader, over little-endian words.
pub fn fletcher32(bytes: &[u8]) -> u32 {
    let fold = |x: u32| (x & 0xffff) + (x >> 16);
    let (a, b) = bytes
        .chunks(718)
        .fold((0xffff_u32, 0xffff_u32), |(a, b), block| {
            let (a, b) = block
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&w| u32::from(u16::from_le_bytes(w)))
                .fold((a, b), |(a, b), w| (a + w, b + a + w));
            (fold(a), fold(b))
        });
    fold(b) << 16 | fold(a)
}
