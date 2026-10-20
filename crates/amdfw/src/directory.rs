// SPDX-License-Identifier: MIT OR Apache-2.0
//! The PSP and BIOS directories: a header and a table of entries, each a firmware file
//! by type. A level-1 directory can point to a level 2, which the PSP then
//! boots from.
use super::Flash;
use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned,
    byteorder::{LittleEndian, U32, U64},
};

pub const PSP_COOKIE: [u8; 4] = *b"$PSP";
pub const PSP_LEVEL2_COOKIE: [u8; 4] = *b"$PL2";
pub const BIOS_COOKIE: [u8; 4] = *b"$BHD";
pub const BIOS_LEVEL2_COOKIE: [u8; 4] = *b"$BL2";

/// PSP directory entry types (`enum amd_fw_type`), the ones fstart names.
pub mod psp {
    pub const NVRAM: u8 = 0x04;
    pub const SOFT_FUSE: u8 = 0x0b;
    pub const SEV_DATA: u8 = 0x38;
    pub const LEVEL2: u8 = 0x40;
    pub const VERSTAGE: u8 = 0x52;
    pub const VERSTAGE_SIGNATURE: u8 = 0x53;
    pub const RPMC_NVRAM: u8 = 0x54;
}

/// BIOS directory entry types (`enum amd_bios_type`).
pub mod bios {
    pub const RTM_PUBKEY: u8 = 0x05;
    pub const SIGNATURE: u8 = 0x07;
    pub const APCB: u8 = 0x60;
    /// The APOB's DRAM destination; no contents.
    pub const APOB: u8 = 0x61;
    /// The reset image, which the PSP copies to DRAM.
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

#[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct Header {
    pub cookie: [u8; 4],
    /// [`fletcher32`] of everything after it: the rest of the header and
    /// the entries.
    pub checksum: U32<LittleEndian>,
    pub count: U32<LittleEndian>,
    /// Version 0: size in 4-KiB blocks (10 bits), SPI block size (4 bits),
    /// base address, address mode.
    pub additional: U32<LittleEndian>,
}

#[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct PspEntry {
    pub kind: u8,
    pub subprogram: u8,
    /// ROM id (2 bits), writable, instance (4 bits).
    pub attributes: [u8; 2],
    pub size: U32<LittleEndian>,
    /// The file's address, or a value (the soft fuses).
    pub address: U64<LittleEndian>,
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
}

#[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct BiosEntry {
    pub kind: u8,
    pub region: u8,
    /// [`BiosEntry::RESET`], [`BiosEntry::COPY`], read-only, compressed,
    /// instance (high 4 bits).
    pub flags: u8,
    /// Subprogram (3 bits), ROM id (2 bits), writable.
    pub subprogram: u8,
    pub size: U32<LittleEndian>,
    pub source: U64<LittleEndian>,
    pub destination: U64<LittleEndian>,
}
impl BiosEntry {
    /// The PSP releases the BSP into this file.
    pub const RESET: u8 = 1 << 0;
    /// The PSP copies the file to its destination.
    pub const COPY: u8 = 1 << 1;

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
            flags: flags | (instance << 4),
            subprogram,
            size: size.into(),
            source: source.into(),
            destination: destination.into(),
        }
    }
}

const _: () = {
    assert!(core::mem::size_of::<Header>() == 16);
    assert!(core::mem::size_of::<PspEntry>() == 16);
    assert!(core::mem::size_of::<BiosEntry>() == 24);
};

/// A directory entry: where its file is and what it is.
pub trait Entry: FromBytes + KnownLayout + Immutable + Unaligned {
    /// The cookies of a level-1 and a level-2 directory.
    const COOKIES: [[u8; 4]; 2];
    /// The type of the entry pointing to the level 2.
    const LEVEL2: u8;
    fn kind(&self) -> u8;
    fn size(&self) -> u32;
    /// The file's address in the flash.
    fn source(&self) -> u64;
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
}

/// A directory in a flash image.
pub struct Directory<'a, E> {
    /// Its flash offset.
    pub offset: usize,
    pub header: &'a Header,
    pub entries: &'a [E],
    table: &'a [u8],
}
impl<E> Clone for Directory<'_, E> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<E> Copy for Directory<'_, E> {}

impl<'a, E: Entry> Directory<'a, E> {
    /// The level-1 (`level` 0) or level-2 (1) directory at flash `offset`,
    /// if its cookie and checksum are right.
    pub fn read(flash: Flash<'a>, offset: usize, level: usize) -> Option<Self> {
        let bytes = flash.bytes().get(offset..)?;
        let (header, rest) = Header::ref_from_prefix(bytes).ok()?;
        let count = usize::try_from(header.count.get()).ok()?;
        let (entries, _) = <[E]>::ref_from_prefix_with_elems(rest, count).ok()?;
        let table = &bytes[..size_of::<Header>() + size_of_val(entries)];
        (header.cookie == E::COOKIES[level] && fletcher32(&table[8..]) == header.checksum.get())
            .then_some(Self {
                offset,
                header,
                entries,
                table,
            })
    }

    /// The header and the entries, as in the flash.
    pub fn table(&self) -> &'a [u8] {
        self.table
    }

    /// The level-2 directory this one points to.
    pub fn level2(&self, flash: Flash<'a>) -> Option<Self> {
        let entry = self.entries.iter().find(|e| e.kind() == E::LEVEL2)?;
        Self::read(flash, flash.offset(entry.source())?, 1)
    }

    /// The entries with contents in `flash`, with their contents. Entries
    /// without (the APOB, which only has a destination) are skipped.
    pub fn files(&self, flash: Flash<'a>) -> impl Iterator<Item = (&'a E, &'a [u8])> + use<'a, E> {
        self.entries
            .iter()
            .filter(|e| e.size() != 0)
            .filter_map(move |e| Some((e, flash.get(e.source(), e.size())?)))
    }
}

/// A level-1 directory and the level 2 it points to.
pub struct Levels<'a, E> {
    pub level1: Directory<'a, E>,
    pub level2: Option<Directory<'a, E>>,
}
impl<'a, E: Entry> Levels<'a, E> {
    pub fn read(flash: Flash<'a>, offset: usize) -> Option<Self> {
        let level1 = Directory::read(flash, offset, 0)?;
        Some(Self {
            level1,
            level2: level1.level2(flash),
        })
    }

    /// The directory the PSP boots from: level 2 if there is one.
    pub fn boot(&self) -> Directory<'a, E> {
        self.level2.unwrap_or(self.level1)
    }

    /// Both levels.
    pub fn iter(&self) -> impl Iterator<Item = Directory<'a, E>> + use<'a, E> {
        core::iter::once(self.level1).chain(self.level2)
    }
}

/// The folded Fletcher-32 over little-endian 16-bit words, in chunks
/// of 359 words (the most that can't overflow). A last odd byte is ignored.
pub fn fletcher32(bytes: &[u8]) -> u32 {
    let fold = |c: u32| (c & 0xffff) + (c >> 16);
    let (c0, c1) = bytes
        .chunks(359 * 2)
        .fold((0xffff, 0xffff), |(c0, c1), chunk| {
            let (c0, c1) = chunk
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&w| u32::from(u16::from_le_bytes(w)))
                .fold((c0, c1), |(c0, c1), word| (c0 + word, c1 + c0 + word));
            (fold(c0), fold(c1))
        });
    (fold(c1) << 16) | fold(c0)
}
