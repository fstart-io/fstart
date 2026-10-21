//! EFS and PSP/BIOS directories (FIRMWARE_IMAGE §1–2).
//!
//! The image is a byte slice starting at the flash offset of the EFS's
//! 16 MiB half: on the host a file, at run time the SPI flash as the FCH
//! maps it (`fch::rom3`). Only the EFS at `0x20000` is populated on SP3.
//! Offsets in the EFS and in mode 0/1 directories are relative to that
//! start and may reach past 16 MiB (AMD #55758 rev 1.13 §4.1.7, "Single
//! 32M in 32MB SPIROM").
//!
//! | | Naples | Rome | Milan |
//! |---|---|---|---|
//! | EFS generation word (`+0x24`) | `ffffffff` (gen1) | `fffffffe` | `fffffffc` |
//! | BIOS directory pointer | `+0x18` | `+0x20` | `+0x28` |
//! | pointers | MMIO (`0xff00_0000 + off`) | offsets, address mode 0 | offsets, address mode 1 |
//!
//! EFS `+0x14` points at a `$PSP` directory or at a `2PSP` combo directory
//! whose entries name one PSP directory per generation (AMD #55758 §4.1.4.2);
//! [`Efs::psp_directories`] picks the generation's entry by its PSP ID.
//! A Naples tree keeps gen1 headers and MMIO pointers even under an EFS
//! shared with a later generation, so "gen1" is a property of the tree's
//! generation, not of the EFS word.
//!
//! Lookups go to the L2 directory first (BIOS `0x70` → `$BL2`, PSP `0x40` →
//! `$PL2`), then L1, as AGESA's APOB lookup (`ffe17a2d`). Every directory
//! must pass its Fletcher-32 checksum.

use core::ops::Range;

use zerocopy::little_endian::{U16, U32, U64};
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

use crate::soc::Generation;

/// EFS offset within a half.
pub const EFS_OFFSET: usize = 0x2_0000;
/// Every offset a Rome/Milan boot ROM checks for an EFS, in search order
/// (AMD #55758 §4.1.7.1/§4.1.7.2); SP3 images only populate `0x20000`.
pub const EFS_SEARCH: [usize; 6] = [
    0xfa_0000, 0xf2_0000, 0xe2_0000, 0xc2_0000, 0x82_0000, EFS_OFFSET,
];
/// EFS signature.
pub const EFS_SIGNATURE: u32 = 0x55aa_55aa;
/// Size of one flash half.
pub const HALF_SIZE: usize = 16 << 20;
/// The half as memory-mapped below 4 GiB.
pub const MMIO_BASE: u64 = 0xff00_0000;
/// Cookie of the PSP combo directory (AMD #55758 Table 10).
pub const PSP_COMBO_COOKIE: [u8; 4] = *b"2PSP";
/// Combo directory header size (cookie, checksum, count, lookup mode, 16
/// reserved bytes) and entry size (`id_sel`, `id`, directory address).
pub const COMBO_HEADER_SIZE: usize = 0x20;
pub const COMBO_ENTRY_SIZE: usize = 0x10;
/// The PSP ID bits a combo entry is matched on: the low byte is free
/// (AMD #55758 Table 12, `0xBC0B00xx`).
pub const PSP_ID_MASK: u32 = 0xffff_ff00;

/// A generation's PSP ID for combo directories (AMD #55758 Table 12).
pub const fn psp_id(generation: Generation) -> u32 {
    match generation {
        Generation::Naples => 0xbc09_0000,
        Generation::Rome => 0xbc0b_0000,
        Generation::Milan => 0xbc0b_0d00,
    }
}

/// BIOS directory entry types used by x86 (FIRMWARE_IMAGE §2).
pub mod bios {
    pub const APCB: u8 = 0x60;
    /// APOB: destination only (`0x3000_0000` on every donor).
    pub const APOB: u8 = 0x61;
    /// The x86 reset image the PSP copies to DRAM.
    pub const RESET_IMAGE: u8 = 0x62;
    /// APOB NV copy (Naples only).
    pub const APOB_NV: u8 = 0x63;
    pub const PMU_CODE: u8 = 0x64;
    pub const PMU_DATA: u8 = 0x65;
    /// Microcode patches, one per instance.
    pub const MICROCODE: u8 = 0x66;
    pub const MCE_DATA: u8 = 0x67;
    pub const APCB_BACKUP: u8 = 0x68;
    /// Pointer to the L2 `$BL2` directory.
    pub const L2: u8 = 0x70;
}

/// PSP directory entry types used here.
pub mod psp {
    /// Soft fuse chain: the "source" is the value.
    pub const SOFT_FUSE: u8 = 0x0b;
    /// Pointer to the L2 `$PL2` directory.
    pub const L2: u8 = 0x40;
}

/// Directory errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirError {
    /// No `0x55aa55aa` at `0x20000`.
    NoEfs,
    /// The EFS generation word does not match the running generation.
    WrongGeneration { word: u32 },
    /// A pointer that does not resolve into the half.
    BadPointer(u64),
    /// An address mode this reader does not implement (2, 3).
    AddressMode(u8),
    /// Wrong cookie at a directory pointer.
    Cookie { offset: usize },
    /// The directory runs past the half.
    Truncated { offset: usize },
    /// Fletcher-32 mismatch.
    Checksum {
        offset: usize,
        stored: u32,
        computed: u32,
    },
    /// The entry has no flash data (a value entry such as APOB).
    NoData,
    /// The combo directory has no entry for this PSP ID.
    NoComboEntry(u32),
}

#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct RawEfs {
    signature: U32,
    imc: U32,
    gbe: U32,
    xhci: U32,
    psp_legacy: U32,
    psp: U32,
    bios_f17_m00: U32,
    bios_f17_m10: U32,
    bios_f17_m30: U32,
    generations: U32,
    bios_f19: U32,
}

/// The embedded firmware structure.
pub struct Efs<'a> {
    image: &'a [u8],
    raw: &'a RawEfs,
}

impl<'a> Efs<'a> {
    /// The EFS of `image` (one 16 MiB half, or a prefix of it).
    pub fn new(image: &'a [u8]) -> Result<Self, DirError> {
        let raw = image
            .get(EFS_OFFSET..)
            .and_then(|b| RawEfs::ref_from_prefix(b).ok())
            .map(|(raw, _)| raw)
            .filter(|raw| raw.signature.get() == EFS_SIGNATURE)
            .ok_or(DirError::NoEfs)?;
        Ok(Self { image, raw })
    }

    /// The EFS generation word (`+0x24`).
    pub fn generation_word(&self) -> u32 {
        self.raw.generations.get()
    }

    /// Gen1 (Naples-only) EFS: bit 0 of the generation word set.
    pub fn is_gen1(&self) -> bool {
        self.generation_word() & 1 != 0
    }

    /// Whether `generation`'s boot ROM accepts this EFS. Rome's requires
    /// bit 0 clear, Milan's bit 1 (`FUSE_MULTI_GEN_EFS_VALUE`, AMD #55758
    /// §4.1.7.1/§4.1.7.2), so a Milan EFS also passes for Rome
    /// (FIRMWARE_IMAGE §1.2). Naples' predates the check and takes the
    /// first EFS of the lower half whatever its word (the AM4 precedent
    /// boots ZP from a `fffffffe` EFS, FIRMWARE_IMAGE §1.2a).
    pub fn matches(&self, generation: Generation) -> bool {
        let word = self.generation_word();
        match generation {
            Generation::Naples => true,
            Generation::Rome => word & 1 == 0,
            Generation::Milan => word & 3 == 0,
        }
    }

    /// The raw BIOS directory pointer of `generation`.
    pub fn bios_pointer(&self, generation: Generation) -> u32 {
        match generation {
            Generation::Naples => self.raw.bios_f17_m00,
            Generation::Rome => self.raw.bios_f17_m30,
            Generation::Milan => self.raw.bios_f19,
        }
        .get()
    }

    /// The raw PSP (or combo) directory pointer (`+0x14`).
    pub fn psp_pointer(&self) -> u32 {
        self.raw.psp.get()
    }

    /// Whether this EFS carries `generation`: its boot ROM accepts it and
    /// the generation's BIOS directory pointer is populated.
    pub fn serves(&self, generation: Generation) -> bool {
        self.matches(generation) && !matches!(self.bios_pointer(generation), 0 | u32::MAX)
    }

    /// The BIOS directories of `generation` (fails if its boot ROM would
    /// not take this EFS).
    pub fn bios_directories(
        &self,
        generation: Generation,
    ) -> Result<Directories<'a, BiosEntry>, DirError> {
        if !self.matches(generation) {
            return Err(DirError::WrongGeneration {
                word: self.generation_word(),
            });
        }
        let gen1 = generation == Generation::Naples;
        let offset = resolve(self.image.len(), self.bios_pointer(generation).into(), gen1)?;
        Directories::new(self.image, offset, gen1)
    }

    /// The PSP directories of `generation`: `+0x14` itself, or the entry of
    /// the `2PSP` combo directory there whose ID is the generation's (the
    /// boot ROM's lookup mode 0 matches on the AMD root key instead, which
    /// differs per generation, FIRMWARE_IMAGE §1.2a).
    pub fn psp_directories(
        &self,
        generation: Generation,
    ) -> Result<Directories<'a, PspEntry>, DirError> {
        let gen1 = generation == Generation::Naples;
        let len = self.image.len();
        let offset = resolve(len, self.psp_pointer().into(), false)?;
        let offset = match Combo::parse(self.image, offset)? {
            None => offset,
            Some(combo) => {
                let wanted = psp_id(generation) & PSP_ID_MASK;
                let entry = combo
                    .entries()
                    .find(|e| e.id & PSP_ID_MASK == wanted)
                    .ok_or(DirError::NoComboEntry(wanted))?;
                resolve(len, entry.directory, gen1)?
            }
        };
        Directories::new(self.image, offset, gen1)
    }
}

/// One `2PSP` combo entry (AMD #55758 Table 11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComboEntry {
    /// 0: compare the PSP ID, 1: the chip family ID.
    pub id_select: u32,
    pub id: u32,
    /// The generation's L1 PSP directory, as a directory pointer.
    pub directory: u64,
}

/// A checked `2PSP` combo directory.
pub struct Combo<'a> {
    bytes: &'a [u8],
}

impl<'a> Combo<'a> {
    /// The combo directory at `offset`, `None` if another cookie is there.
    pub fn parse(image: &'a [u8], offset: usize) -> Result<Option<Self>, DirError> {
        let truncated = DirError::Truncated { offset };
        let header = image
            .get(offset..offset + COMBO_HEADER_SIZE)
            .ok_or(truncated)?;
        if header[..4] != PSP_COMBO_COOKIE {
            return Ok(None);
        }
        let count = u32::from_le_bytes(header[8..12].try_into().unwrap()) as usize;
        let bytes = count
            .checked_mul(COMBO_ENTRY_SIZE)
            .and_then(|n| image.get(offset..offset + COMBO_HEADER_SIZE + n))
            .ok_or(truncated)?;
        let stored = u32::from_le_bytes(header[4..8].try_into().unwrap());
        let computed = fletcher32(&bytes[8..]);
        if stored != computed {
            return Err(DirError::Checksum {
                offset,
                stored,
                computed,
            });
        }
        Ok(Some(Self { bytes }))
    }

    /// `LookUpMode`: 0 = try every entry against the AMD root key hash,
    /// 1 = match the PSP/chip ID.
    pub fn lookup_mode(&self) -> u32 {
        u32::from_le_bytes(self.bytes[12..16].try_into().unwrap())
    }

    pub fn entries(&self) -> impl Iterator<Item = ComboEntry> + '_ {
        self.bytes[COMBO_HEADER_SIZE..]
            .as_chunks::<COMBO_ENTRY_SIZE>()
            .0
            .iter()
            .map(|e| ComboEntry {
                id_select: u32::from_le_bytes(e[0..4].try_into().unwrap()),
                id: u32::from_le_bytes(e[4..8].try_into().unwrap()),
                directory: u64::from_le_bytes(e[8..16].try_into().unwrap()),
            })
    }
}

/// The offset of the 16 MiB half whose EFS `generation`'s boot ROM takes
/// from `flash` (the whole part from offset 0): the lower half, then the
/// upper one; Naples searches only the lower half (FIRMWARE_IMAGE §1.2,
/// AMD #55758 §4.1.7). Only `0x20000` is checked: the image builder
/// refuses a signature at any other search offset.
pub fn booted_half(flash: &[u8], generation: Generation) -> Option<usize> {
    let halves: &[usize] = match generation {
        Generation::Naples => &[0],
        Generation::Rome | Generation::Milan => &[0, HALF_SIZE],
    };
    halves.iter().copied().find(|&half| {
        flash
            .get(half..)
            .and_then(|image| Efs::new(image).ok())
            .is_some_and(|efs| efs.matches(generation))
    })
}

/// A pointer in the EFS or a mode 0/1 directory: an MMIO address in the
/// window below 4 GiB, or (gen2) an offset into the half.
fn resolve(len: usize, value: u64, gen1: bool) -> Result<usize, DirError> {
    let offset = match value {
        v if (MMIO_BASE..1 << 32).contains(&v) => v - MMIO_BASE,
        v if !gen1 => v,
        v => return Err(DirError::BadPointer(v)),
    };
    usize::try_from(offset)
        .ok()
        .filter(|&o| o < len)
        .ok_or(DirError::BadPointer(value))
}

/// Fletcher-32 over little-endian 16-bit words, folded every 359 words
/// (amdfwtool's directory checksum).
pub fn fletcher32(data: &[u8]) -> u32 {
    let fold = |x: u32| (x & 0xffff) + (x >> 16);
    let (c0, c1) = data
        .chunks(2 * 359)
        .fold((0xffff_u32, 0xffff_u32), |(c0, c1), block| {
            let (c0, c1) = block
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&w| u32::from(u16::from_le_bytes(w)))
                .fold((c0, c1), |(c0, c1), w| (c0 + w, c1 + c0 + w));
            (fold(c0), fold(c1))
        });
    fold(c1) << 16 | fold(c0)
}

#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct DirHeader {
    cookie: [u8; 4],
    checksum: U32,
    count: U32,
    info: U32,
}

/// A directory entry layout.
pub trait Entry: FromBytes + KnownLayout + Immutable + Unaligned {
    /// L1 and L2 cookies.
    const COOKIES: [[u8; 4]; 2];
    /// The entry type pointing to the L2 directory.
    const L2: u8;
    fn kind(&self) -> u8;
    fn size(&self) -> u32;
    fn source(&self) -> u64;
    fn instance(&self) -> u8;
    fn subprogram(&self) -> u8;
}

/// PSP directory entry (16 bytes).
#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
pub struct PspEntry {
    kind: u8,
    subprogram: u8,
    flags: U16,
    size: U32,
    source: U64,
}

impl PspEntry {
    pub fn rom_id(&self) -> u8 {
        (self.flags.get() & 3) as u8
    }

    pub fn writable(&self) -> bool {
        self.flags.get() & 4 != 0
    }
}

impl Entry for PspEntry {
    const COOKIES: [[u8; 4]; 2] = [*b"$PSP", *b"$PL2"];
    const L2: u8 = psp::L2;

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
        (self.flags.get() >> 3 & 0xf) as u8
    }
    fn subprogram(&self) -> u8 {
        self.subprogram
    }
}

/// BIOS directory entry (24 bytes).
#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
pub struct BiosEntry {
    kind: u8,
    region: u8,
    flags: u8,
    sub: u8,
    size: U32,
    source: U64,
    destination: U64,
}

impl BiosEntry {
    pub fn region(&self) -> u8 {
        self.region
    }
    /// The PSP copies this image and releases x86 into it.
    pub fn reset(&self) -> bool {
        self.flags & 1 != 0
    }
    /// Copied to `destination` by the PSP.
    pub fn copy(&self) -> bool {
        self.flags & 2 != 0
    }
    pub fn read_only(&self) -> bool {
        self.flags & 4 != 0
    }
    pub fn compressed(&self) -> bool {
        self.flags & 8 != 0
    }
    pub fn rom_id(&self) -> u8 {
        self.sub >> 3 & 3
    }
    /// DRAM destination (`u64::MAX` = none).
    pub fn destination(&self) -> u64 {
        self.destination.get()
    }
}

impl Entry for BiosEntry {
    const COOKIES: [[u8; 4]; 2] = [*b"$BHD", *b"$BL2"];
    const L2: u8 = bios::L2;

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
        self.sub & 7
    }
}

/// One checked directory.
pub struct Directory<'a, E> {
    image: &'a [u8],
    offset: usize,
    header: &'a DirHeader,
    entries: &'a [E],
    gen1: bool,
}

impl<'a, E: Entry> Directory<'a, E> {
    fn parse(image: &'a [u8], offset: usize, gen1: bool) -> Result<Self, DirError> {
        let truncated = DirError::Truncated { offset };
        let bytes = image.get(offset..).ok_or(truncated)?;
        let (header, rest) = DirHeader::ref_from_prefix(bytes).map_err(|_| truncated)?;
        if !E::COOKIES.contains(&header.cookie) {
            return Err(DirError::Cookie { offset });
        }
        let count = header.count.get() as usize;
        let (entries, _) = <[E]>::ref_from_prefix_with_elems(rest, count).map_err(|_| truncated)?;
        let body = &bytes[8..16 + core::mem::size_of_val(entries)];
        let computed = fletcher32(body);
        if computed != header.checksum.get() {
            return Err(DirError::Checksum {
                offset,
                stored: header.checksum.get(),
                computed,
            });
        }
        Ok(Self {
            image,
            offset,
            header,
            entries,
            gen1,
        })
    }

    /// Offset of the directory in the half.
    pub fn offset(&self) -> usize {
        self.offset
    }

    pub fn cookie(&self) -> [u8; 4] {
        self.header.cookie
    }

    pub fn entries(&self) -> &'a [E] {
        self.entries
    }

    /// Gen2 address mode (additional info bits 30:29); gen1 has none (0).
    pub fn address_mode(&self) -> u8 {
        if self.gen1 {
            0
        } else {
            (self.header.info.get() >> 29 & 3) as u8
        }
    }

    /// The flash range of `entry`'s data within the half.
    ///
    /// Modes 0 (physical/offset) and 1 (offset from the start of the half)
    /// resolve the same way within a half; modes 2/3 (directory and slot
    /// relative) are not used by any SP3 donor and are refused.
    pub fn range(&self, entry: &E) -> Result<Range<usize>, DirError> {
        let mode = self.address_mode().max((entry.source() >> 62) as u8);
        if mode > 1 {
            return Err(DirError::AddressMode(mode));
        }
        let size = match entry.size() {
            0 | u32::MAX => return Err(DirError::NoData),
            s => s as usize,
        };
        let source = entry.source() & !(3 << 62);
        let start = resolve(self.image.len(), source, self.gen1)?;
        let end = start
            .checked_add(size)
            .filter(|&e| e <= self.image.len())
            .ok_or(DirError::BadPointer(source))?;
        Ok(start..end)
    }

    /// `entry`'s data.
    pub fn data(&self, entry: &E) -> Result<&'a [u8], DirError> {
        self.range(entry).map(|r| &self.image[r])
    }

    fn l2(&self) -> Result<Option<Self>, DirError> {
        self.entries
            .iter()
            .find(|e| e.kind() == E::L2)
            .map(|e| {
                let range = self.range(e)?;
                Self::parse(self.image, range.start, self.gen1)
            })
            .transpose()
    }
}

/// An L1 directory and the L2 it points to.
pub struct Directories<'a, E> {
    pub l1: Directory<'a, E>,
    pub l2: Option<Directory<'a, E>>,
}

impl<'a, E: Entry> Directories<'a, E> {
    fn new(image: &'a [u8], offset: usize, gen1: bool) -> Result<Self, DirError> {
        let l1 = Directory::parse(image, offset, gen1)?;
        let l2 = l1.l2()?;
        Ok(Self { l1, l2 })
    }

    /// L2 first, then L1.
    pub fn levels(&self) -> impl Iterator<Item = &Directory<'a, E>> {
        self.l2.iter().chain(core::iter::once(&self.l1))
    }

    /// The first entry of `kind` (L2 first) matching `instance` and
    /// `subprogram` when given, with its directory.
    pub fn find(
        &self,
        kind: u8,
        instance: Option<u8>,
        subprogram: Option<u8>,
    ) -> Option<(&Directory<'a, E>, &'a E)> {
        self.levels().find_map(|dir| {
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

    /// Every entry of `kind` from the first level that has one (L2 first),
    /// e.g. all microcode instances.
    pub fn all(&self, kind: u8) -> impl Iterator<Item = (&Directory<'a, E>, &'a E)> {
        let dir = self
            .levels()
            .find(|d| d.entries.iter().any(|e| e.kind() == kind));
        dir.into_iter().flat_map(move |d| {
            d.entries
                .iter()
                .filter(move |e| e.kind() == kind)
                .map(move |e| (d, e))
        })
    }
}

impl Directories<'_, BiosEntry> {
    /// The reset image: `(flash range, DRAM destination)`.
    pub fn reset_image(&self) -> Result<(Range<usize>, u64), DirError> {
        let (dir, e) = self
            .find(bios::RESET_IMAGE, None, None)
            .ok_or(DirError::NoData)?;
        Ok((dir.range(e)?, e.destination()))
    }

    /// The APOB DRAM address (entry 0x61 destination).
    pub fn apob_address(&self) -> Option<u64> {
        self.find(bios::APOB, None, None)
            .map(|(_, e)| e.destination())
            .filter(|&d| d != u64::MAX)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A donor half, if the SP3 analysis workspace is next to this tree.
    pub(crate) fn donor(name: &str) -> Option<Vec<u8>> {
        let dir = std::env::var_os("SP3_DONORS")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../sp3/firmware/donors")
            });
        std::fs::read(dir.join(format!("{name}.bin"))).ok()
    }

    fn bios_entry(kind: u8, flags: u8, size: u32, source: u64, destination: u64) -> Vec<u8> {
        let mut e = vec![kind, 0, flags, 0];
        e.extend(size.to_le_bytes());
        e.extend(source.to_le_bytes());
        e.extend(destination.to_le_bytes());
        e
    }

    fn put_dir(image: &mut [u8], offset: usize, cookie: &[u8; 4], info: u32, entries: &[Vec<u8>]) {
        let mut d = cookie.to_vec();
        d.extend([0; 4]);
        d.extend((entries.len() as u32).to_le_bytes());
        d.extend(info.to_le_bytes());
        entries.iter().for_each(|e| d.extend(e));
        let sum = fletcher32(&d[8..]);
        d[4..8].copy_from_slice(&sum.to_le_bytes());
        image[offset..offset + d.len()].copy_from_slice(&d);
    }

    /// A Milan-style half: EFS, `$BHD` with APOB, reset image and an L2
    /// with two microcode patches.
    pub(crate) fn synthetic_milan() -> Vec<u8> {
        let mut img = vec![0xff; 0x40000];
        let efs = [
            EFS_SIGNATURE,
            0,
            0,
            0,
            0,
            0x30000,
            0,
            0,
            0,
            0xffff_fffc,
            0x31000,
        ];
        efs.iter()
            .enumerate()
            .for_each(|(i, w)| img[EFS_OFFSET + 4 * i..][..4].copy_from_slice(&w.to_le_bytes()));
        let mode1 = 1 << 29;
        put_dir(
            &mut img,
            0x31000,
            b"$BHD",
            mode1,
            &[
                bios_entry(bios::APOB, 0, 0, 0, 0x3000_0000),
                bios_entry(bios::RESET_IMAGE, 3, 0x8000, 0x38000, 0x75ff_8000),
                bios_entry(bios::L2, 0, 0x1000, 0x32000, u64::MAX),
            ],
        );
        put_dir(
            &mut img,
            0x32000,
            b"$BL2",
            mode1,
            &[
                bios_entry(bios::MICROCODE, 0, 0x15c0, 0x33000, u64::MAX),
                bios_entry(bios::MICROCODE, 1 << 4, 0x15c0, 0x35000, u64::MAX),
                bios_entry(bios::RESET_IMAGE, 3, 0x8000, 0x38000, 0x75ff_8000),
            ],
        );
        put_dir(&mut img, 0x30000, b"$PSP", mode1, &[]);
        img
    }

    #[test]
    fn fletcher_known_value() {
        // Values of the analysis workspace's `tools.build_firmware_image.fletcher32`;
        // 2000 bytes cross the 359-word fold.
        let data: Vec<u8> = (0..=255).collect();
        assert_eq!(fletcher32(&data), 0x7555_3fc0);
        let data: Vec<u8> = (0..2000u32).map(|i| (i * 7) as u8).collect();
        assert_eq!(fletcher32(&data), 0xc496_b05b);
    }

    #[test]
    fn synthetic_lookup() {
        let img = synthetic_milan();
        let efs = Efs::new(&img).unwrap();
        assert!(
            efs.matches(Generation::Rome),
            "Milan EFS passes Rome's check"
        );
        assert!(efs.serves(Generation::Milan) && !efs.serves(Generation::Rome));
        assert!(!efs.serves(Generation::Naples), "no Naples BIOS directory");
        let mut rome = img.clone();
        rome[EFS_OFFSET + 0x24..][..4].copy_from_slice(&0xffff_fffe_u32.to_le_bytes());
        assert!(matches!(
            Efs::new(&rome).unwrap().bios_directories(Generation::Milan),
            Err(DirError::WrongGeneration { word: 0xffff_fffe })
        ));
        let dirs = efs.bios_directories(Generation::Milan).unwrap();
        assert_eq!(dirs.l1.address_mode(), 1);
        assert_eq!(dirs.apob_address(), Some(0x3000_0000));
        assert_eq!(dirs.reset_image().unwrap(), (0x38000..0x40000, 0x75ff_8000));
        let (dir, _) = dirs.find(bios::RESET_IMAGE, None, None).unwrap();
        assert_eq!(dir.cookie(), *b"$BL2", "L2 first");
        let ucode: Vec<_> = dirs
            .all(bios::MICROCODE)
            .map(|(d, e)| d.range(e).unwrap().start)
            .collect();
        assert_eq!(ucode, [0x33000, 0x35000]);
        assert!(dirs.find(bios::MICROCODE, Some(1), None).is_some());
        assert!(dirs.find(bios::MICROCODE, Some(2), None).is_none());
        let (dir, apob) = dirs.find(bios::APOB, None, None).unwrap();
        assert_eq!(dir.range(apob), Err(DirError::NoData));
        assert_eq!(
            efs.psp_directories(Generation::Milan)
                .unwrap()
                .l1
                .entries()
                .len(),
            0
        );
    }

    /// A `2PSP` at `0x3f000` naming the `$PSP` at `0x30000` for Milan.
    #[test]
    fn combo_directory_selects_by_psp_id() {
        let mut img = synthetic_milan();
        let mut combo = PSP_COMBO_COOKIE.to_vec();
        combo.extend([0; 4]);
        combo.extend(2u32.to_le_bytes());
        combo.extend([0; 20]);
        for (id, dir) in [(0xbc0b_0000u32, 0x3e000u64), (0xbc0b_0d00, 0x30000)] {
            combo.extend(0u32.to_le_bytes());
            combo.extend(id.to_le_bytes());
            combo.extend(dir.to_le_bytes());
        }
        let sum = fletcher32(&combo[8..]);
        combo[4..8].copy_from_slice(&sum.to_le_bytes());
        img[0x3f000..0x3f000 + combo.len()].copy_from_slice(&combo);
        img[EFS_OFFSET + 0x14..][..4].copy_from_slice(&0x3f000u32.to_le_bytes());
        let efs = Efs::new(&img).unwrap();
        let parsed = Combo::parse(&img, 0x3f000).unwrap().unwrap();
        assert_eq!(parsed.lookup_mode(), 0);
        assert_eq!(parsed.entries().count(), 2);
        assert_eq!(
            efs.psp_directories(Generation::Milan).unwrap().l1.offset(),
            0x30000
        );
        // Rome's entry points at erased flash.
        assert!(matches!(
            efs.psp_directories(Generation::Rome),
            Err(DirError::Cookie { offset: 0x3e000 })
        ));
        assert!(matches!(
            efs.psp_directories(Generation::Naples),
            Err(DirError::NoComboEntry(0xbc09_0000))
        ));
        img[0x3f000 + 0x24] ^= 1;
        assert!(matches!(
            Efs::new(&img).unwrap().psp_directories(Generation::Milan),
            Err(DirError::Checksum {
                offset: 0x3f000,
                ..
            })
        ));
    }

    #[test]
    fn boot_rom_takes_the_first_accepted_half() {
        let milan = synthetic_milan();
        let mut flash = vec![0xff; 2 * HALF_SIZE];
        flash[HALF_SIZE..HALF_SIZE + milan.len()].copy_from_slice(&milan);
        assert_eq!(booted_half(&flash, Generation::Milan), Some(HALF_SIZE));
        assert_eq!(booted_half(&flash, Generation::Naples), None);
        flash[..milan.len()].copy_from_slice(&milan);
        assert_eq!(booted_half(&flash, Generation::Rome), Some(0));
        // Naples ignores the generation word.
        assert_eq!(booted_half(&flash, Generation::Naples), Some(0));
    }

    #[test]
    fn checksum_is_enforced() {
        let mut img = synthetic_milan();
        img[0x32000 + 16] ^= 1;
        let efs = Efs::new(&img).unwrap();
        assert!(matches!(
            efs.bios_directories(Generation::Milan),
            Err(DirError::Checksum {
                offset: 0x32000,
                ..
            })
        ));
    }

    /// (donor, generation, reset flash, size, destination, microcode count)
    const DONORS: &[(&str, Generation, usize, usize, u64, usize)] = &[
        (
            "naples-s8026-v202",
            Generation::Naples,
            0xdb_0000,
            0x25_0000,
            0x75db_0000,
            2,
        ),
        (
            "rome-s8030-v404",
            Generation::Rome,
            0xdb_0000,
            0x25_0000,
            0x75db_0000,
            2,
        ),
        (
            "rome-s8026-v202",
            Generation::Rome,
            0xdb_0000,
            0x25_0000,
            0x75db_0000,
            2,
        ),
        (
            "milan-s8030-v404",
            Generation::Milan,
            0xce_0000,
            0x32_0000,
            0x75ce_0000,
            4,
        ),
    ];

    #[test]
    fn donor_directories() {
        for &(name, generation, flash, size, dest, ucode) in DONORS {
            let Some(img) = donor(name) else {
                eprintln!("skipping {name}: donor absent");
                continue;
            };
            let efs = Efs::new(&img).unwrap();
            let dirs = efs.bios_directories(generation).unwrap();
            assert!(dirs.l2.is_some(), "{name}");
            assert_eq!(
                dirs.reset_image().unwrap(),
                (flash..flash + size, dest),
                "{name}"
            );
            assert_eq!(dirs.apob_address(), Some(0x3000_0000), "{name}");
            let patches: Vec<_> = dirs
                .all(bios::MICROCODE)
                .map(|(d, e)| d.data(e).unwrap())
                .collect();
            assert_eq!(patches.len(), ucode, "{name}");
            let nv = dirs
                .find(bios::APOB_NV, None, None)
                .map(|(d, e)| d.range(e).unwrap());
            match generation {
                Generation::Naples => assert_eq!(nv, Some(0x24_e000..0x25_e000)),
                _ => assert_eq!(nv, None, "{name}"),
            }
            assert!(
                dirs.find(
                    bios::PMU_CODE,
                    Some(1),
                    Some(if generation == Generation::Naples {
                        0
                    } else {
                        1
                    })
                )
                .is_some()
            );
            let psp = efs.psp_directories(generation).unwrap();
            assert!(psp.l2.is_some(), "{name}");
            assert!(psp.find(psp::SOFT_FUSE, None, None).is_some());
        }
        // 2P donors only have to parse.
        for (name, generation) in [
            ("naples2p-knppd32-1801", Generation::Naples),
            ("rome2p-s8253-v203", Generation::Rome),
            ("milan2p-s8253-v203", Generation::Milan),
        ] {
            if let Some(img) = donor(name) {
                let dirs = Efs::new(&img)
                    .unwrap()
                    .bios_directories(generation)
                    .unwrap();
                assert!(dirs.reset_image().is_ok(), "{name}");
            }
        }
    }
}
