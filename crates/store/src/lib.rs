//! Firmware store: tagged, persistent firmware data in one RAM window.
//!
//! Like coreboot's CBMEM, the store keeps data that outlives a stage —
//! S3 stage caches, the memory-training record, ACPI and SMBIOS tables,
//! timestamps — as ID-tagged entries in one platform-chosen window. The
//! platform picks the window base; the used size is decided at boot and
//! the platform hands everything above it back to the OS.
//!
//! The wire format is the Arm Firmware Handoff transfer list (TL, spec
//! v1.0, version 1), the format TF-A, OP-TEE and U-Boot use, so later
//! stages built on those projects can read it. fstart entries use tags
//! from the spec's non-standard range (see [`tag`]). The checksum is not
//! used: entries such as the timestamp table are updated in place.
//!
//! Allocation only appends. Entries are addressed by their offset from
//! the list base, which [`Store::relocate`] preserves.

#![no_std]

use core::ptr::NonNull;
use zerocopy::little_endian::U32;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

#[cfg(test)]
mod tests;

/// TL header signature.
pub const SIGNATURE: u32 = 0x4a0f_b10b;
/// TL format version written and accepted.
pub const VERSION: u8 = 1;
/// TL header size, also the offset of the first entry.
pub const HEADER_SIZE: usize = 0x18;
/// Transfer entry header size.
pub const ENTRY_HEADER_SIZE: usize = 8;
/// Entries start at multiples of this.
const GRANULE: usize = 8;
/// `flags` bit: the checksum covers the used bytes.
const FLAG_HAS_CHECKSUM: u32 = 1 << 0;

#[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct ListHeader {
    signature: U32,
    checksum: u8,
    version: u8,
    header_size: u8,
    /// log2 of the largest entry-data alignment; the base must honour it.
    alignment: u8,
    used_size: U32,
    max_size: U32,
    flags: U32,
    reserved: U32,
}

#[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct EntryHeader {
    tag: [u8; 3],
    header_size: u8,
    data_size: U32,
}

const _: () = assert!(size_of::<ListHeader>() == HEADER_SIZE);
const _: () = assert!(size_of::<EntryHeader>() == ENTRY_HEADER_SIZE);

/// A 24-bit transfer entry tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tag(u32);

impl Tag {
    /// Padding; never returned by lookups.
    pub const VOID: Self = Self(0);

    /// Tag `index` within the spec's non-standard range `0xfff000..`.
    pub const fn vendor(index: u16) -> Self {
        assert!(index < 0x1000);
        Self(0xff_f000 + index as u32)
    }

    pub const fn value(self) -> u32 {
        self.0
    }

    fn from_bytes(bytes: [u8; 3]) -> Self {
        Self(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], 0]))
    }

    fn to_bytes(self) -> [u8; 3] {
        let [a, b, c, _] = self.0.to_le_bytes();
        [a, b, c]
    }
}

/// fstart's entry tags, the counterpart of coreboot's `cbmem_id.h`.
pub mod tag {
    use super::Tag;

    /// Boot timestamp table.
    pub const TIMESTAMPS: Tag = Tag::vendor(0x000);
    /// In-memory console log.
    pub const CONSOLE: Tag = Tag::vendor(0x001);
    /// S3 copy of the stored postcar body.
    pub const STAGE_CACHE_POSTCAR: Tag = Tag::vendor(0x002);
    /// S3 copy of the stored ramstage body.
    pub const STAGE_CACHE_RAMSTAGE: Tag = Tag::vendor(0x003);
    /// Memory-training record awaiting its flash commit.
    pub const TRAINING: Tag = Tag::vendor(0x004);
    /// ACPI tables, starting with the RSDP.
    pub const ACPI: Tag = Tag::vendor(0x005);
    /// SMBIOS entry point and structure table.
    pub const SMBIOS: Tag = Tag::vendor(0x006);
    /// Raminit's installed-DRAM inventory for SMBIOS.
    pub const MEMORY_INFO: Tag = Tag::vendor(0x007);
    /// coreboot table and the files handed to a coreboot payload.
    pub const COREBOOT: Tag = Tag::vendor(0x008);
}

/// Location of one entry's data within its store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    tag: Tag,
    /// Data offset from the list base.
    offset: usize,
    size: usize,
}

impl Entry {
    pub const fn tag(&self) -> Tag {
        self.tag
    }

    /// Data size in bytes.
    pub const fn len(&self) -> usize {
        self.size
    }

    pub const fn is_empty(&self) -> bool {
        self.size == 0
    }

    /// End of this entry's footprint, which is where the next entry starts.
    fn end(&self) -> usize {
        self.offset + self.size.next_multiple_of(GRANULE)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The base breaks the requested or recorded alignment.
    Misaligned,
    /// The window cannot hold the list or its recorded maximum.
    TooSmall,
    /// No valid transfer list at this base.
    Invalid,
    /// The entry does not fit below the maximum size.
    Full,
    /// Only the last entry can be resized.
    NotLast,
    /// Void or out-of-range tag.
    BadTag,
}

/// A transfer list in firmware-owned memory.
pub struct Store {
    base: NonNull<u8>,
}

impl Store {
    /// Write an empty list at `base` that may grow to `window` bytes.
    ///
    /// Entry data is not cleared.
    ///
    /// # Safety
    ///
    /// `base..base + window` is writable memory owned by the store for as
    /// long as the store and its entries are used.
    pub unsafe fn create(base: NonNull<u8>, window: usize) -> Result<Self, Error> {
        if !(base.as_ptr() as usize).is_multiple_of(GRANULE) {
            return Err(Error::Misaligned);
        }
        let max = (window.min(u32::MAX as usize) / GRANULE) * GRANULE;
        if max < HEADER_SIZE {
            return Err(Error::TooSmall);
        }
        let store = Self { base };
        store.write_header(&ListHeader {
            signature: SIGNATURE.into(),
            checksum: 0,
            version: VERSION,
            header_size: HEADER_SIZE as u8,
            alignment: GRANULE.trailing_zeros() as u8,
            used_size: (HEADER_SIZE as u32).into(),
            max_size: (max as u32).into(),
            flags: 0.into(),
            reserved: 0.into(),
        });
        Ok(store)
    }

    /// Open a list previously written at `base`, never trusting it beyond
    /// `window` bytes.
    ///
    /// # Safety
    ///
    /// `base..base + window` is readable, writable memory owned by the
    /// store for as long as the store and its entries are used.
    pub unsafe fn open(base: NonNull<u8>, window: usize) -> Result<Self, Error> {
        if !(base.as_ptr() as usize).is_multiple_of(GRANULE) {
            return Err(Error::Misaligned);
        }
        if window < HEADER_SIZE {
            return Err(Error::TooSmall);
        }
        let store = Self { base };
        let header = store.header();
        let (used, max) = (
            header.used_size.get() as usize,
            header.max_size.get() as usize,
        );
        let header_size = header.header_size as usize;
        if header.signature.get() != SIGNATURE
            || header.version != VERSION
            || header_size < HEADER_SIZE
            || !header_size.is_multiple_of(GRANULE)
            || used < header_size
            || used % GRANULE != 0
            || used > max
        {
            return Err(Error::Invalid);
        }
        if max > window {
            return Err(Error::TooSmall);
        }
        let align_mask = 1usize
            .checked_shl(header.alignment.into())
            .ok_or(Error::Invalid)?
            - 1;
        if base.as_ptr() as usize & align_mask != 0 {
            return Err(Error::Misaligned);
        }
        if header.flags.get() & FLAG_HAS_CHECKSUM != 0 {
            let sum = (0..used).fold(0u8, |sum, i| sum.wrapping_add(store.byte(i)));
            if sum != 0 {
                return Err(Error::Invalid);
            }
        }
        // The entry chain must end exactly at the used size.
        let mut offset = header_size;
        while offset < used {
            let entry = store.entry_at(offset).ok_or(Error::Invalid)?;
            offset = entry.end();
        }
        if offset != used {
            return Err(Error::Invalid);
        }
        Ok(store)
    }

    /// Physical address of the list header.
    pub fn base(&self) -> usize {
        self.base.as_ptr() as usize
    }

    /// Bytes in use, header included.
    pub fn used(&self) -> usize {
        self.header().used_size.get() as usize
    }

    /// Size the list may grow to.
    pub fn max_size(&self) -> usize {
        self.header().max_size.get() as usize
    }

    /// Address of an entry's data.
    pub fn address(&self, entry: &Entry) -> usize {
        self.base() + entry.offset
    }

    /// An entry's data.
    ///
    /// # Safety
    ///
    /// The entry belongs to this list and nothing else accesses its data
    /// while the returned slice lives.
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn bytes_mut(&self, entry: &Entry) -> &mut [u8] {
        // SAFETY: the entry lies within the used size the caller owns; the
        // caller guarantees exclusive access to its data.
        unsafe { core::slice::from_raw_parts_mut(self.base.as_ptr().add(entry.offset), entry.size) }
    }

    /// Entries in list order, padding excluded.
    pub fn entries(&self) -> impl Iterator<Item = Entry> + '_ {
        let used = self.used();
        let mut offset = self.header().header_size as usize;
        core::iter::from_fn(move || {
            while offset < used {
                let entry = self.entry_at(offset)?;
                offset = entry.end();
                if entry.tag != Tag::VOID {
                    return Some(entry);
                }
            }
            None
        })
    }

    /// First entry with `tag`.
    pub fn find(&self, tag: Tag) -> Option<Entry> {
        self.entries().find(|entry| entry.tag == tag)
    }

    /// Append an entry of `size` bytes whose data is aligned to
    /// `1 << align_log2` bytes. The data is not initialized.
    pub fn add(&mut self, tag: Tag, size: usize, align_log2: u8) -> Result<Entry, Error> {
        if tag == Tag::VOID || tag.0 >= 1 << 24 {
            return Err(Error::BadTag);
        }
        let align = 1usize
            .checked_shl(align_log2.into())
            .ok_or(Error::Misaligned)?
            .max(GRANULE);
        if !self.base().is_multiple_of(align) {
            return Err(Error::Misaligned);
        }
        let used = self.used();
        // Pad with a void entry until the new entry's data is aligned.
        let padding =
            (used + ENTRY_HEADER_SIZE).next_multiple_of(align) - (used + ENTRY_HEADER_SIZE);
        let offset = used + padding + ENTRY_HEADER_SIZE;
        let data_size = u32::try_from(size).map_err(|_| Error::Full)?;
        let end = offset
            .checked_add(size.next_multiple_of(GRANULE))
            .ok_or(Error::Full)?;
        if end > self.max_size() {
            return Err(Error::Full);
        }
        if padding != 0 {
            self.write_entry_header(used, Tag::VOID, padding - ENTRY_HEADER_SIZE);
        }
        self.write_entry_header(offset - ENTRY_HEADER_SIZE, tag, data_size as usize);
        let mut header = self.header();
        header.used_size = (end as u32).into();
        header.alignment = header.alignment.max(align.trailing_zeros() as u8);
        self.write_header(&header);
        Ok(Entry { tag, offset, size })
    }

    /// Grow or shrink the last entry to `size` bytes.
    pub fn resize_last(&mut self, entry: Entry, size: usize) -> Result<Entry, Error> {
        if self.entry_at(entry.offset - ENTRY_HEADER_SIZE) != Some(entry)
            || entry.end() != self.used()
        {
            return Err(Error::NotLast);
        }
        let resized = Entry { size, ..entry };
        if size > u32::MAX as usize || resized.end() > self.max_size() {
            return Err(Error::Full);
        }
        self.write_entry_header(entry.offset - ENTRY_HEADER_SIZE, entry.tag, size);
        let mut header = self.header();
        header.used_size = (resized.end() as u32).into();
        self.write_header(&header);
        Ok(resized)
    }

    /// Stop growth at the used size rounded up to `granule`, which must be
    /// a power of two. Returns the new maximum: the extent the platform
    /// keeps reserved, starting at [`base`](Self::base).
    pub fn limit(&mut self, granule: usize) -> usize {
        let mut header = self.header();
        let limited = self
            .used()
            .next_multiple_of(granule.max(GRANULE))
            .min(header.max_size.get() as usize);
        header.max_size = (limited as u32).into();
        self.write_header(&header);
        limited
    }

    /// Copy the list to `dest` with room to grow to `window` bytes.
    /// Entries keep their offsets; existing [`Entry`] values stay valid
    /// for the returned store.
    ///
    /// # Safety
    ///
    /// As for [`create`](Self::create); `dest` does not overlap this list.
    pub unsafe fn relocate(&self, dest: NonNull<u8>, window: usize) -> Result<Self, Error> {
        let header = self.header();
        let align = 1usize << header.alignment;
        if !(dest.as_ptr() as usize).is_multiple_of(align) {
            return Err(Error::Misaligned);
        }
        let max = (window.min(u32::MAX as usize) / GRANULE) * GRANULE;
        let used = self.used();
        if max < used {
            return Err(Error::TooSmall);
        }
        // SAFETY: both ranges are owned by their stores and do not overlap.
        unsafe { core::ptr::copy_nonoverlapping(self.base.as_ptr(), dest.as_ptr(), used) };
        let moved = Self { base: dest };
        let mut header = moved.header();
        header.max_size = (max as u32).into();
        moved.write_header(&header);
        Ok(moved)
    }

    fn byte(&self, offset: usize) -> u8 {
        // SAFETY: callers stay below the validated used size.
        unsafe { self.base.as_ptr().add(offset).read() }
    }

    fn read<T: FromBytes>(&self, offset: usize) -> T {
        // SAFETY: callers read headers inside the owned window.
        let bytes =
            unsafe { core::slice::from_raw_parts(self.base.as_ptr().add(offset), size_of::<T>()) };
        T::read_from_bytes(bytes).unwrap_or_else(|_| unreachable!())
    }

    fn write<T: IntoBytes + Immutable>(&self, offset: usize, value: &T) {
        let bytes = value.as_bytes();
        // SAFETY: callers write headers inside the owned window.
        unsafe {
            core::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                self.base.as_ptr().add(offset),
                bytes.len(),
            )
        };
    }

    fn header(&self) -> ListHeader {
        self.read(0)
    }

    fn write_header(&self, header: &ListHeader) {
        self.write(0, header);
    }

    /// The entry whose header starts at `offset`, if it lies within the
    /// used size.
    fn entry_at(&self, offset: usize) -> Option<Entry> {
        let used = self.used();
        if !offset.is_multiple_of(GRANULE) || offset.checked_add(ENTRY_HEADER_SIZE)? > used {
            return None;
        }
        let header: EntryHeader = self.read(offset);
        let header_size = header.header_size as usize;
        if header_size < ENTRY_HEADER_SIZE || !header_size.is_multiple_of(GRANULE) {
            return None;
        }
        let entry = Entry {
            tag: Tag::from_bytes(header.tag),
            offset: offset.checked_add(header_size)?,
            size: header.data_size.get() as usize,
        };
        (entry
            .offset
            .checked_add(entry.size.next_multiple_of(GRANULE))?
            <= used)
            .then_some(entry)
    }

    fn write_entry_header(&self, offset: usize, tag: Tag, data_size: usize) {
        self.write(
            offset,
            &EntryHeader {
                tag: tag.to_bytes(),
                header_size: ENTRY_HEADER_SIZE as u8,
                data_size: (data_size as u32).into(),
            },
        );
    }
}
