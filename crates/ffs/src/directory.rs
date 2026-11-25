//! Authenticated directory revision 2, retained in an immutable RAM buffer.
//!
//! Wire definition: every integer is LE, every record is packed without padding.
//! The 28-byte header is `FSMZ`, u32 revision=2, then u32 region/entry/segment
//! counts, string-table offset and string-table byte length. Tables follow in
//! that order, then NUL-terminated UTF-8 strings; trailing bytes are rejected.
//! String offsets are relative to the string table, not the directory.
//!
//! Region (24 bytes): five u32 (name, media offset, size, first entry, count),
//! u8 kind (container=1/raw=2), fill, two reserved zero bytes.
//! Entry (88 bytes): five u32 (name, region-relative offset, size, first segment,
//! count), u8 file type, kind (file=1/raw=2), fill, digest flags, SHA256[32],
//! legacy SHA3[32]. File digest flags must be exactly 1; SHA3 storage is zero.
//! File-type IDs: stage=1, board-config=2, payload=3, FDT=4, data=5, raw=6,
//! firmware=7, FIT=8, microcode=9, initramfs=10.
//! Segment (100 bytes): five u32 (name, entry-relative offset, stored length,
//! memory length, disjoint workspace size), u64 load address; u8 kind
//! (code=1/ro=2/rw=3/BSS=4), compression (none=0/LZ4-block=1/LZMA-Alone=2), flags
//! (execute=1/write=2/read=4), reserved zero; u32 initialized length;
//! stored SHA256[32], initialized SHA256[32]. Digests are mandatory fixed fields
//! for stored segments. Pure BSS has zero stored/initialized lengths and hashes.
//! Initialized length excludes the zero-filled tail of memory length.
//!
//! Names are identities within a named region; unqualified lookup rejects
//! ambiguity. Regions only describe media layout, never write authority.
//! Parsing validates structure, not provenance: verify the exact stable buffer
//! using the authenticated root's DirectoryRef before exposing file views.
//! Golden revision fixture: `tests/fixtures/directory-v2.bin`.

use crate::reader::ReaderError;
use fstart_core::ffs::{
    Compression, DigestSet, EntryContent, FileType, ImageManifest, Region, RegionContent,
    RegionEntry, Segment, SegmentFlags, SegmentKind,
};
use heapless::String as HString;
use zerocopy::byteorder::{LE, U32, U64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Ref, Unaligned};

const MAGIC: u32 = u32::from_le_bytes(*b"FSMZ");
const VERSION: u32 = crate::root::DIRECTORY_REVISION;

#[derive(Clone, Copy, Debug, FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned)]
#[repr(C)]
struct Header {
    magic: U32<LE>,
    version: U32<LE>,
    region_count: U32<LE>,
    entry_count: U32<LE>,
    segment_count: U32<LE>,
    string_table_offset: U32<LE>,
    string_table_size: U32<LE>,
}

#[derive(Clone, Copy, Debug, FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned)]
#[repr(C)]
struct RegionRecord {
    name_offset: U32<LE>,
    offset: U32<LE>,
    size: U32<LE>,
    first_entry: U32<LE>,
    entry_count: U32<LE>,
    kind: u8,
    fill: u8,
    _reserved: [u8; 2],
}

#[derive(Clone, Copy, Debug, FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned)]
#[repr(C)]
struct EntryRecord {
    name_offset: U32<LE>,
    offset: U32<LE>,
    size: U32<LE>,
    first_segment: U32<LE>,
    segment_count: U32<LE>,
    file_type: u8,
    kind: u8,
    fill: u8,
    digest_flags: u8,
    sha256: [u8; 32],
    sha3_256: [u8; 32],
}

#[derive(Clone, Copy, Debug, FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned)]
#[repr(C)]
pub struct SegmentRecord {
    name_offset: U32<LE>,
    offset: U32<LE>,
    stored_size: U32<LE>,
    loaded_size: U32<LE>,
    in_place_size: U32<LE>,
    load_addr: U64<LE>,
    kind: u8,
    compression: u8,
    flags: u8,
    _reserved: u8,
    initialized_size: U32<LE>,
    stored_digest: [u8; 32],
    loaded_digest: [u8; 32],
}

const REGION_KIND_CONTAINER: u8 = 1;
const REGION_KIND_RAW: u8 = 2;
const ENTRY_KIND_FILE: u8 = 1;
const ENTRY_KIND_RAW: u8 = 2;
const DIGEST_SHA256: u8 = 1 << 0;
const DIGEST_SHA3_256: u8 = 1 << 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirectorySummary {
    pub regions: usize,
    pub entries: usize,
}

#[derive(Clone, Copy)]
pub struct DirectoryView<'a> {
    regions: &'a [RegionRecord],
    entries: &'a [EntryRecord],
    segments: &'a [SegmentRecord],
    strings: &'a [u8],
}

#[derive(Clone, Copy)]
pub struct FileView<'a> {
    entry: &'a EntryRecord,
    region_offset: u32,
    segments: &'a [SegmentRecord],
    strings: &'a [u8],
}

impl<'a> DirectoryView<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, ReaderError> {
        let (header_ref, _rest) =
            Ref::<_, Header>::from_prefix(bytes).map_err(|_| ReaderError::DeserializeError)?;
        let header = *header_ref;
        if header.magic.get() != MAGIC || header.version.get() != VERSION {
            return Err(ReaderError::UnsupportedVersion);
        }

        let header_size = core::mem::size_of::<Header>();
        let region_count = header.region_count.get() as usize;
        let entry_count = header.entry_count.get() as usize;
        let segment_count = header.segment_count.get() as usize;
        let strings_offset = header.string_table_offset.get() as usize;
        let strings_size = header.string_table_size.get() as usize;

        let region_size = core::mem::size_of::<RegionRecord>()
            .checked_mul(region_count)
            .ok_or(ReaderError::OutOfBounds)?;
        let entry_offset = header_size
            .checked_add(region_size)
            .ok_or(ReaderError::OutOfBounds)?;
        let entry_size = core::mem::size_of::<EntryRecord>()
            .checked_mul(entry_count)
            .ok_or(ReaderError::OutOfBounds)?;
        let segment_offset = entry_offset
            .checked_add(entry_size)
            .ok_or(ReaderError::OutOfBounds)?;
        let segment_size = core::mem::size_of::<SegmentRecord>()
            .checked_mul(segment_count)
            .ok_or(ReaderError::OutOfBounds)?;
        let expected_strings_offset = segment_offset
            .checked_add(segment_size)
            .ok_or(ReaderError::OutOfBounds)?;
        if strings_offset != expected_strings_offset {
            return Err(ReaderError::DeserializeError);
        }
        let strings_end = strings_offset
            .checked_add(strings_size)
            .ok_or(ReaderError::OutOfBounds)?;
        if strings_end != bytes.len() {
            return Err(ReaderError::OutOfBounds);
        }

        let region_bytes = bytes
            .get(header_size..entry_offset)
            .ok_or(ReaderError::OutOfBounds)?;
        let entry_bytes = bytes
            .get(entry_offset..segment_offset)
            .ok_or(ReaderError::OutOfBounds)?;
        let segment_bytes = bytes
            .get(segment_offset..expected_strings_offset)
            .ok_or(ReaderError::OutOfBounds)?;
        let strings = bytes
            .get(strings_offset..strings_end)
            .ok_or(ReaderError::OutOfBounds)?;

        let regions_ref =
            Ref::<_, [RegionRecord]>::from_bytes_with_elems(region_bytes, region_count)
                .map_err(|_| ReaderError::DeserializeError)?;
        let entries_ref = Ref::<_, [EntryRecord]>::from_bytes_with_elems(entry_bytes, entry_count)
            .map_err(|_| ReaderError::DeserializeError)?;
        let segments_ref =
            Ref::<_, [SegmentRecord]>::from_bytes_with_elems(segment_bytes, segment_count)
                .map_err(|_| ReaderError::DeserializeError)?;
        let regions = Ref::into_ref(regions_ref);
        let entries = Ref::into_ref(entries_ref);
        let segments = Ref::into_ref(segments_ref);

        let view = Self {
            regions,
            entries,
            segments,
            strings,
        };
        view.validate()?;
        Ok(view)
    }

    fn validate(&self) -> Result<(), ReaderError> {
        self.validate_tables()?;
        self.validate_regions()?;
        self.validate_files()?;
        self.validate_partition()?;
        self.validate_segments()
    }

    /// Record kinds, names and child ranges: everything lookups index by.
    fn validate_tables(&self) -> Result<(), ReaderError> {
        for region in self.regions {
            self.string(region.name_offset.get())?;
            match region.kind {
                REGION_KIND_CONTAINER => {
                    let first = region.first_entry.get() as usize;
                    let count = region.entry_count.get() as usize;
                    checked_range(self.entries.len(), first, count)?;
                }
                REGION_KIND_RAW => {}
                _ => return Err(ReaderError::DeserializeError),
            }
        }
        for entry in self.entries {
            self.string(entry.name_offset.get())?;
            match entry.kind {
                ENTRY_KIND_FILE => {
                    decode_file_type(entry.file_type)?;
                    let first = entry.first_segment.get() as usize;
                    let count = entry.segment_count.get() as usize;
                    checked_range(self.segments.len(), first, count)?;
                }
                ENTRY_KIND_RAW => {}
                _ => return Err(ReaderError::DeserializeError),
            }
        }
        Ok(())
    }

    /// Region extents, unique region names and unique, contained children.
    fn validate_regions(&self) -> Result<(), ReaderError> {
        for (i, region) in self.regions.iter().enumerate() {
            if region._reserved != [0; 2]
                || region.offset.get().checked_add(region.size.get()).is_none()
            {
                return Err(ReaderError::OutOfBounds);
            }
            for prev in &self.regions[..i] {
                if self.string(prev.name_offset.get())? == self.string(region.name_offset.get())? {
                    return Err(ReaderError::DeserializeError);
                }
            }
            if region.kind == REGION_KIND_CONTAINER {
                let first = region.first_entry.get() as usize;
                let entries = &self.entries[first..first + region.entry_count.get() as usize];
                for (i, entry) in entries.iter().enumerate() {
                    for prev in &entries[..i] {
                        if self.string(prev.name_offset.get())?
                            == self.string(entry.name_offset.get())?
                        {
                            return Err(ReaderError::DeserializeError);
                        }
                    }
                    checked_range(
                        region.size.get() as usize,
                        entry.offset.get() as usize,
                        entry.size.get() as usize,
                    )?;
                }
            }
        }
        Ok(())
    }

    /// File digest policy and segments within their file.
    fn validate_files(&self) -> Result<(), ReaderError> {
        for entry in self.entries {
            if entry.kind != ENTRY_KIND_FILE {
                continue;
            }
            if entry.segment_count.get() == 0
                || entry.digest_flags != DIGEST_SHA256
                || entry.sha3_256 != [0; 32]
                || entry.fill != 0
            {
                return Err(ReaderError::DeserializeError);
            }
            let first = entry.first_segment.get() as usize;
            for s in &self.segments[first..first + entry.segment_count.get() as usize] {
                checked_range(
                    entry.size.get() as usize,
                    s.offset() as usize,
                    s.stored_size() as usize,
                )?;
            }
        }
        Ok(())
    }

    /// Tables partition their children exactly; aliasing records or orphan
    /// records would make lookup identity and authenticated ranges ambiguous.
    fn validate_partition(&self) -> Result<(), ReaderError> {
        for i in 0..self.entries.len() {
            if self
                .regions
                .iter()
                .filter(|r| {
                    r.kind == REGION_KIND_CONTAINER
                        && i >= r.first_entry.get() as usize
                        && i < r.first_entry.get() as usize + r.entry_count.get() as usize
                })
                .count()
                != 1
            {
                return Err(ReaderError::DeserializeError);
            }
        }
        for i in 0..self.segments.len() {
            if self
                .entries
                .iter()
                .filter(|e| {
                    e.kind == ENTRY_KIND_FILE
                        && i >= e.first_segment.get() as usize
                        && i < e.first_segment.get() as usize + e.segment_count.get() as usize
                })
                .count()
                != 1
            {
                return Err(ReaderError::DeserializeError);
            }
        }
        Ok(())
    }

    /// Segment names, kinds, compression, sizes and digest invariants.
    fn validate_segments(&self) -> Result<(), ReaderError> {
        for segment in self.segments {
            self.string(segment.name_offset.get())?;
            let kind = segment.kind()?;
            let compression = segment.compression()?;
            if segment._reserved != 0
                || segment.flags & !7 != 0
                || segment.initialized_size() > segment.loaded_size()
                || segment
                    .load_addr()
                    .checked_add(u64::from(segment.loaded_size()))
                    .is_none()
            {
                return Err(ReaderError::DeserializeError);
            }
            if compression == Compression::None && segment.in_place_size() != 0 {
                return Err(ReaderError::DeserializeError);
            }
            if compression != Compression::None
                && segment.in_place_size()
                    < segment
                        .initialized_size()
                        .checked_add(segment.stored_size())
                        .ok_or(ReaderError::OutOfBounds)?
            {
                return Err(ReaderError::DeserializeError);
            }
            if segment.stored_size() == 0 {
                if kind != SegmentKind::Bss
                    || segment.initialized_size() != 0
                    || compression != Compression::None
                    || segment.stored_digest != [0; 32]
                    || segment.loaded_digest != [0; 32]
                {
                    return Err(ReaderError::DeserializeError);
                }
            } else {
                if kind == SegmentKind::Bss || segment.initialized_size() == 0 {
                    return Err(ReaderError::DeserializeError);
                }
                if compression == Compression::None
                    && (segment.stored_size() != segment.initialized_size()
                        || segment.stored_digest != segment.loaded_digest)
                {
                    return Err(ReaderError::DeserializeError);
                }
            }
        }
        Ok(())
    }

    pub fn summary(&self) -> DirectorySummary {
        DirectorySummary {
            regions: self.regions.len(),
            entries: self.entries.len(),
        }
    }

    pub fn find_file_by_type(&self, file_type: FileType) -> Result<FileView<'a>, ReaderError> {
        let mut found = None;
        for region in self.regions {
            if region.kind != REGION_KIND_CONTAINER {
                continue;
            }
            let first = region.first_entry.get() as usize;
            let count = region.entry_count.get() as usize;
            for entry in &self.entries[first..first + count] {
                if entry.kind == ENTRY_KIND_FILE && entry.file_type == encode_file_type(file_type) {
                    if found.is_some() {
                        return Err(ReaderError::DeserializeError);
                    }
                    found = Some(self.file_view(region, entry)?);
                }
            }
        }
        found.ok_or(ReaderError::FileNotFound)
    }

    pub fn find_file_by_name(&self, name: &str) -> Result<FileView<'a>, ReaderError> {
        let mut found = None;
        for region in self.regions {
            if region.kind != REGION_KIND_CONTAINER {
                continue;
            }
            let first = region.first_entry.get() as usize;
            let count = region.entry_count.get() as usize;
            for entry in &self.entries[first..first + count] {
                if entry.kind == ENTRY_KIND_FILE && self.string(entry.name_offset.get())? == name {
                    if found.is_some() {
                        return Err(ReaderError::DeserializeError);
                    }
                    found = Some(self.file_view(region, entry)?);
                }
            }
        }
        found.ok_or(ReaderError::FileNotFound)
    }

    fn file_view(
        &self,
        region: &'a RegionRecord,
        entry: &'a EntryRecord,
    ) -> Result<FileView<'a>, ReaderError> {
        let first = entry.first_segment.get() as usize;
        let count = entry.segment_count.get() as usize;
        checked_range(self.segments.len(), first, count)?;
        Ok(FileView {
            entry,
            region_offset: region.offset.get(),
            segments: &self.segments[first..first + count],
            strings: self.strings,
        })
    }

    fn string(&self, offset: u32) -> Result<&'a str, ReaderError> {
        string_at(self.strings, offset)
    }

    /// Bounded raw-region lookup without decoding the owned directory tree.
    /// Duplicate names or a container masquerading as the reservation fail.
    pub fn raw_region(&self, name: &str) -> Result<Option<(u32, u32, u8)>, ReaderError> {
        let mut found = None;
        for region in self.regions {
            if self.string(region.name_offset.get())? == name {
                if found.is_some() || region.kind != REGION_KIND_RAW {
                    return Err(ReaderError::DeserializeError);
                }
                found = Some((region.offset.get(), region.size.get(), region.fill));
            }
        }
        Ok(found)
    }

    pub fn to_owned_manifest(&self) -> Result<ImageManifest, ReaderError> {
        let mut regions = heapless::Vec::new();
        for region in self.regions {
            let name = hstring64(self.string(region.name_offset.get())?)?;
            let content = match region.kind {
                REGION_KIND_CONTAINER => {
                    let first = region.first_entry.get() as usize;
                    let count = region.entry_count.get() as usize;
                    let mut children = heapless::Vec::new();
                    for entry in &self.entries[first..first + count] {
                        children
                            .push(self.entry_to_owned(entry)?)
                            .map_err(|_| ReaderError::DeserializeError)?;
                    }
                    RegionContent::Container { children }
                }
                REGION_KIND_RAW => RegionContent::Raw { fill: region.fill },
                _ => return Err(ReaderError::DeserializeError),
            };
            regions
                .push(Region {
                    name,
                    offset: region.offset.get(),
                    size: region.size.get(),
                    content,
                })
                .map_err(|_| ReaderError::DeserializeError)?;
        }
        Ok(ImageManifest { regions })
    }

    fn entry_to_owned(&self, entry: &EntryRecord) -> Result<RegionEntry, ReaderError> {
        let name = hstring64(self.string(entry.name_offset.get())?)?;
        let content = match entry.kind {
            ENTRY_KIND_FILE => {
                let first = entry.first_segment.get() as usize;
                let count = entry.segment_count.get() as usize;
                let mut segments = heapless::Vec::new();
                for segment in &self.segments[first..first + count] {
                    segments
                        .push(segment_to_owned(self.strings, segment)?)
                        .map_err(|_| ReaderError::DeserializeError)?;
                }
                EntryContent::File {
                    file_type: decode_file_type(entry.file_type)?,
                    segments,
                    digests: digest_set(entry),
                }
            }
            ENTRY_KIND_RAW => EntryContent::Raw { fill: entry.fill },
            _ => return Err(ReaderError::DeserializeError),
        };
        Ok(RegionEntry {
            name,
            offset: entry.offset.get(),
            size: entry.size.get(),
            content,
        })
    }
}

impl<'a> FileView<'a> {
    pub fn file_type(&self) -> Result<FileType, ReaderError> {
        decode_file_type(self.entry.file_type)
    }

    pub fn name(&self) -> Result<&'a str, ReaderError> {
        string_at(self.strings, self.entry.name_offset.get())
    }

    pub fn region_offset(&self) -> u32 {
        self.region_offset
    }

    pub fn entry_offset(&self) -> u32 {
        self.entry.offset.get()
    }

    pub fn segments(&self) -> &'a [SegmentRecord] {
        self.segments
    }

    pub fn digests(&self) -> DigestSet {
        digest_set(self.entry)
    }
}

impl SegmentRecord {
    pub fn name<'a>(&self, strings: &'a [u8]) -> Result<&'a str, ReaderError> {
        string_at(strings, self.name_offset.get())
    }

    pub fn offset(&self) -> u32 {
        self.offset.get()
    }

    pub fn initialized_size(&self) -> u32 {
        self.initialized_size.get()
    }

    pub fn stored_digest(&self) -> [u8; 32] {
        self.stored_digest
    }

    pub fn loaded_digest(&self) -> [u8; 32] {
        self.loaded_digest
    }

    pub fn flags(&self) -> SegmentFlags {
        decode_segment_flags(self.flags)
    }

    pub fn stored_size(&self) -> u32 {
        self.stored_size.get()
    }

    pub fn loaded_size(&self) -> u32 {
        self.loaded_size.get()
    }

    pub fn in_place_size(&self) -> u32 {
        self.in_place_size.get()
    }

    pub fn load_addr(&self) -> u64 {
        self.load_addr.get()
    }

    pub fn kind(&self) -> Result<SegmentKind, ReaderError> {
        decode_segment_kind(self.kind)
    }

    pub fn compression(&self) -> Result<Compression, ReaderError> {
        decode_compression(self.compression)
    }
}

#[cfg(feature = "std")]
pub fn encode_manifest(
    manifest: &ImageManifest,
) -> Result<alloc::vec::Vec<u8>, alloc::string::String> {
    use alloc::string::ToString;
    use alloc::vec::Vec;

    let mut regions = Vec::<RegionRecord>::new();
    let mut entries = Vec::<EntryRecord>::new();
    let mut segments = Vec::<SegmentRecord>::new();
    let mut strings = Vec::<u8>::new();

    for region in &manifest.regions {
        let name_offset = push_string(&mut strings, region.name.as_str())?;
        let first_entry = entries.len() as u32;
        let (kind, fill, entry_count) = match &region.content {
            RegionContent::Container { children } => {
                for entry in children {
                    encode_entry(entry, &mut entries, &mut segments, &mut strings)?;
                }
                (REGION_KIND_CONTAINER, 0, children.len() as u32)
            }
            RegionContent::Raw { fill } => (REGION_KIND_RAW, *fill, 0),
        };
        regions.push(RegionRecord {
            name_offset: U32::new(name_offset),
            offset: U32::new(region.offset),
            size: U32::new(region.size),
            first_entry: U32::new(first_entry),
            entry_count: U32::new(entry_count),
            kind,
            fill,
            _reserved: [0; 2],
        });
    }

    let header_size = core::mem::size_of::<Header>();
    let regions_size = regions.len() * core::mem::size_of::<RegionRecord>();
    let entries_size = entries.len() * core::mem::size_of::<EntryRecord>();
    let segments_size = segments.len() * core::mem::size_of::<SegmentRecord>();
    let string_table_offset = header_size + regions_size + entries_size + segments_size;
    let header = Header {
        magic: U32::new(MAGIC),
        version: U32::new(VERSION),
        region_count: U32::new(
            regions
                .len()
                .try_into()
                .map_err(|_| "too many regions".to_string())?,
        ),
        entry_count: U32::new(
            entries
                .len()
                .try_into()
                .map_err(|_| "too many entries".to_string())?,
        ),
        segment_count: U32::new(
            segments
                .len()
                .try_into()
                .map_err(|_| "too many segments".to_string())?,
        ),
        string_table_offset: U32::new(
            string_table_offset
                .try_into()
                .map_err(|_| "manifest too large".to_string())?,
        ),
        string_table_size: U32::new(
            strings
                .len()
                .try_into()
                .map_err(|_| "string table too large".to_string())?,
        ),
    };

    let mut out = Vec::with_capacity(string_table_offset + strings.len());
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(regions.as_bytes());
    out.extend_from_slice(entries.as_bytes());
    out.extend_from_slice(segments.as_bytes());
    out.extend_from_slice(&strings);
    DirectoryView::parse(&out).map_err(|e| alloc::format!("invalid directory: {e:?}"))?;
    Ok(out)
}

#[cfg(feature = "std")]
fn encode_entry(
    entry: &RegionEntry,
    entries: &mut alloc::vec::Vec<EntryRecord>,
    segments: &mut alloc::vec::Vec<SegmentRecord>,
    strings: &mut alloc::vec::Vec<u8>,
) -> Result<(), alloc::string::String> {
    let name_offset = push_string(strings, entry.name.as_str())?;
    let first_segment = segments.len() as u32;
    let (kind, fill, file_type, digest_flags, sha256, sha3_256, segment_count) =
        match &entry.content {
            EntryContent::File {
                file_type,
                segments: entry_segments,
                digests,
            } => {
                for segment in entry_segments {
                    encode_segment(segment, segments, strings)?;
                }
                let mut sha256 = [0; 32];
                let mut sha3_256 = [0; 32];
                let mut digest_flags = 0;
                if let Some(digest) = digests.sha256 {
                    sha256 = digest;
                    digest_flags |= DIGEST_SHA256;
                }
                if let Some(digest) = digests.sha3_256 {
                    sha3_256 = digest;
                    digest_flags |= DIGEST_SHA3_256;
                }
                (
                    ENTRY_KIND_FILE,
                    0,
                    encode_file_type(*file_type),
                    digest_flags,
                    sha256,
                    sha3_256,
                    entry_segments.len() as u32,
                )
            }
            EntryContent::Raw { fill } => (ENTRY_KIND_RAW, *fill, 0, 0, [0; 32], [0; 32], 0),
        };
    entries.push(EntryRecord {
        name_offset: U32::new(name_offset),
        offset: U32::new(entry.offset),
        size: U32::new(entry.size),
        first_segment: U32::new(first_segment),
        segment_count: U32::new(segment_count),
        file_type,
        kind,
        fill,
        digest_flags,
        sha256,
        sha3_256,
    });
    Ok(())
}

#[cfg(feature = "std")]
fn encode_segment(
    segment: &Segment,
    segments: &mut alloc::vec::Vec<SegmentRecord>,
    strings: &mut alloc::vec::Vec<u8>,
) -> Result<(), alloc::string::String> {
    let name_offset = push_string(strings, segment.name.as_str())?;
    segments.push(SegmentRecord {
        name_offset: U32::new(name_offset),
        offset: U32::new(segment.offset),
        stored_size: U32::new(segment.stored_size),
        loaded_size: U32::new(segment.loaded_size),
        in_place_size: U32::new(segment.in_place_size),
        load_addr: U64::new(segment.load_addr),
        kind: encode_segment_kind(segment.kind),
        compression: encode_compression(segment.compression),
        flags: encode_segment_flags(segment.flags),
        _reserved: 0,
        initialized_size: U32::new(segment.initialized_size),
        stored_digest: segment.stored_digest,
        loaded_digest: segment.loaded_digest,
    });
    Ok(())
}

#[cfg(feature = "std")]
fn push_string(strings: &mut alloc::vec::Vec<u8>, s: &str) -> Result<u32, alloc::string::String> {
    use alloc::string::ToString;
    if s.is_empty() || s.as_bytes().contains(&0) {
        return Err("directory names must be nonempty and contain no NUL".into());
    }
    let offset = strings.len();
    strings.extend_from_slice(s.as_bytes());
    strings.push(0);
    offset
        .try_into()
        .map_err(|_| "string table too large".to_string())
}

fn checked_range(len: usize, start: usize, count: usize) -> Result<(), ReaderError> {
    let end = start.checked_add(count).ok_or(ReaderError::OutOfBounds)?;
    if end <= len {
        Ok(())
    } else {
        Err(ReaderError::OutOfBounds)
    }
}

fn string_at(strings: &[u8], offset: u32) -> Result<&str, ReaderError> {
    let start = offset as usize;
    let tail = strings.get(start..).ok_or(ReaderError::OutOfBounds)?;
    let len = tail
        .iter()
        .position(|&b| b == 0)
        .ok_or(ReaderError::DeserializeError)?;
    core::str::from_utf8(&tail[..len]).map_err(|_| ReaderError::DeserializeError)
}

fn digest_set(entry: &EntryRecord) -> DigestSet {
    DigestSet {
        sha256: (entry.digest_flags & DIGEST_SHA256 != 0).then_some(entry.sha256),
        sha3_256: (entry.digest_flags & DIGEST_SHA3_256 != 0).then_some(entry.sha3_256),
    }
}

fn hstring64(s: &str) -> Result<HString<64>, ReaderError> {
    HString::try_from(s).map_err(|_| ReaderError::DeserializeError)
}

fn hstring32(s: &str) -> Result<HString<32>, ReaderError> {
    HString::try_from(s).map_err(|_| ReaderError::DeserializeError)
}

fn segment_to_owned(strings: &[u8], segment: &SegmentRecord) -> Result<Segment, ReaderError> {
    Ok(Segment {
        name: hstring32(segment.name(strings)?)?,
        kind: segment.kind()?,
        offset: segment.offset(),
        stored_size: segment.stored_size(),
        initialized_size: segment.initialized_size(),
        stored_digest: segment.stored_digest(),
        loaded_digest: segment.loaded_digest(),
        loaded_size: segment.loaded_size(),
        in_place_size: segment.in_place_size(),
        load_addr: segment.load_addr(),
        compression: segment.compression()?,
        flags: decode_segment_flags(segment.flags),
    })
}

pub fn encode_file_type(file_type: FileType) -> u8 {
    match file_type {
        FileType::StageCode => 1,
        FileType::BoardConfig => 2,
        FileType::Payload => 3,
        FileType::Fdt => 4,
        FileType::Data => 5,
        FileType::Raw => 6,
        FileType::Firmware => 7,
        FileType::FitImage => 8,
        FileType::CpuMicrocode => 9,
        FileType::Initramfs => 10,
    }
}

fn decode_file_type(value: u8) -> Result<FileType, ReaderError> {
    match value {
        1 => Ok(FileType::StageCode),
        2 => Ok(FileType::BoardConfig),
        3 => Ok(FileType::Payload),
        4 => Ok(FileType::Fdt),
        5 => Ok(FileType::Data),
        6 => Ok(FileType::Raw),
        7 => Ok(FileType::Firmware),
        8 => Ok(FileType::FitImage),
        9 => Ok(FileType::CpuMicrocode),
        10 => Ok(FileType::Initramfs),
        _ => Err(ReaderError::DeserializeError),
    }
}

#[cfg(feature = "std")]
fn encode_segment_kind(kind: SegmentKind) -> u8 {
    match kind {
        SegmentKind::Code => 1,
        SegmentKind::ReadOnlyData => 2,
        SegmentKind::ReadWriteData => 3,
        SegmentKind::Bss => 4,
    }
}

fn decode_segment_kind(value: u8) -> Result<SegmentKind, ReaderError> {
    match value {
        1 => Ok(SegmentKind::Code),
        2 => Ok(SegmentKind::ReadOnlyData),
        3 => Ok(SegmentKind::ReadWriteData),
        4 => Ok(SegmentKind::Bss),
        _ => Err(ReaderError::DeserializeError),
    }
}

#[cfg(feature = "std")]
fn encode_compression(compression: Compression) -> u8 {
    match compression {
        Compression::None => 0,
        Compression::Lz4 => 1,
        Compression::Lzma => 2,
    }
}

#[cfg(feature = "std")]
fn encode_segment_flags(flags: SegmentFlags) -> u8 {
    (flags.execute as u8) | ((flags.write as u8) << 1) | ((flags.read as u8) << 2)
}

fn decode_segment_flags(flags: u8) -> SegmentFlags {
    SegmentFlags {
        execute: flags & 1 != 0,
        write: flags & 2 != 0,
        read: flags & 4 != 0,
    }
}

fn decode_compression(value: u8) -> Result<Compression, ReaderError> {
    match value {
        0 => Ok(Compression::None),
        1 => Ok(Compression::Lz4),
        2 => Ok(Compression::Lzma),
        _ => Err(ReaderError::DeserializeError),
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    const HEADER: usize = core::mem::size_of::<Header>();
    const REGION: usize = core::mem::size_of::<RegionRecord>();
    const ENTRY: usize = core::mem::size_of::<EntryRecord>();
    const SEGMENT: usize = core::mem::size_of::<SegmentRecord>();

    /// The real lookup: in bounds, NUL-terminated, no interior NUL.
    #[kani::proof]
    #[kani::unwind(8)]
    fn verify_string_at_is_bounded_and_nul_terminated() {
        let strings: [u8; 6] = kani::any();
        let offset: u32 = kani::any();
        if let Ok(name) = string_at(&strings, offset) {
            kani::cover!(!name.is_empty(), "non-empty name");
            let start = offset as usize;
            let end = start + name.len();
            assert!(end < strings.len());
            assert_eq!(strings[end], 0);
            assert!(name.as_bytes() == &strings[start..end]);
            assert!(!name.as_bytes().contains(&0));
        }
    }

    /// Table contents are `validate`'s job, proven phase by phase below.
    // `'a: 'a` makes the lifetime early-bound, matching the impl's generic.
    #[allow(clippy::extra_unused_lifetimes)]
    fn validate_any<'a>(_: &DirectoryView<'a>) -> Result<(), ReaderError>
    where
        'a: 'a,
    {
        if kani::any() {
            Ok(())
        } else {
            Err(ReaderError::DeserializeError)
        }
    }

    /// Room for one record of each table plus a short string table.
    const SMALL: usize = HEADER + REGION + ENTRY + SEGMENT + 8;

    /// Arbitrary header values: all count, offset and size arithmetic is
    /// checked, and the tables exactly partition the buffer.
    #[kani::proof]
    #[kani::stub(DirectoryView::validate, validate_any)]
    fn verify_directory_parse_never_panics_on_arbitrary_bytes() {
        let bytes: [u8; SMALL] = kani::any();
        let len: usize = kani::any_where(|&l| l <= SMALL);
        if let Ok(view) = DirectoryView::parse(&bytes[..len]) {
            kani::cover!(
                !view.regions.is_empty() && !view.entries.is_empty() && !view.segments.is_empty(),
                "all three tables present"
            );
            let size = HEADER
                + view.regions.len() * REGION
                + view.entries.len() * ENTRY
                + view.segments.len() * SEGMENT
                + view.strings.len();
            assert_eq!(size, len);
            assert_eq!(
                view.summary(),
                DirectorySummary {
                    regions: view.regions.len(),
                    entries: view.entries.len(),
                }
            );
        }
    }

    const R: usize = 2;
    const E: usize = 2;
    const S: usize = 2;
    /// `x\0y\0` for any ASCII `x`, `y` spells two distinct non-empty names
    /// (`a\0b\0`), duplicates (`a\0a\0`) and empty names (`\0` at either
    /// offset), while every NUL search ends within two bytes, so all loops
    /// stay within an unwinding bound of 3. Unterminated and non-ASCII
    /// strings are `string_at`'s own proof above.
    const STRINGS: usize = 4;

    fn any_string_table() -> [u8; STRINGS] {
        [
            kani::any_where(u8::is_ascii),
            0,
            kani::any_where(u8::is_ascii),
            0,
        ]
    }

    const ASCII_BYTES: [u8; 128] = {
        let mut bytes = [0u8; 128];
        let mut i = 0;
        while i < bytes.len() {
            bytes[i] = i as u8;
            i += 1;
        }
        bytes
    };
    /// Every ASCII character, in order, validated at compile time.
    const ASCII: &str = match core::str::from_utf8(&ASCII_BYTES) {
        Ok(s) => s,
        Err(_) => panic!(),
    };

    /// `core::str::from_utf8` on at most one ASCII byte, the only strings the
    /// lookup harnesses decode: the same text (proven below), taken from
    /// [`ASCII`] so no UTF-8 validator is unrolled. Any other input fails
    /// the proof instead of being modelled.
    fn from_utf8_ascii(v: &[u8]) -> Result<&str, core::str::Utf8Error> {
        match *v {
            [] => Ok(""),
            [b] if b.is_ascii() => Ok(&ASCII[b as usize..b as usize + 1]),
            _ => panic!("outside the modelled UTF-8 domain"),
        }
    }

    /// The real validator agrees with [`from_utf8_ascii`] on its domain.
    #[kani::proof]
    #[kani::unwind(3)]
    fn verify_from_utf8_ascii_matches_core() {
        let bytes: [u8; 1] = [kani::any_where(u8::is_ascii)];
        let len: usize = kani::any_where(|&l| l <= 1);
        let v = &bytes[..len];
        let s = core::str::from_utf8(v).unwrap();
        assert!(core::ptr::eq(s.as_bytes(), v));
        assert_eq!(Ok(s), from_utf8_ascii(v));
    }

    /// Region and entry names must decode for `validate_tables` to accept,
    /// so offsets past the table are only ever rejected; the harnesses below
    /// assume acceptance and skip generating them. (Segment names are only
    /// checked by the segment phase and stay unconstrained.)
    fn any_name_offset() -> U32<LE> {
        U32::new(kani::any_where(|&o: &u32| (o as usize) < STRINGS))
    }

    fn any_region() -> RegionRecord {
        RegionRecord {
            name_offset: any_name_offset(),
            offset: U32::new(kani::any()),
            size: U32::new(kani::any()),
            first_entry: U32::new(kani::any()),
            entry_count: U32::new(kani::any()),
            kind: kani::any(),
            fill: kani::any(),
            _reserved: kani::any(),
        }
    }

    fn any_entry() -> EntryRecord {
        EntryRecord {
            name_offset: any_name_offset(),
            offset: U32::new(kani::any()),
            size: U32::new(kani::any()),
            first_segment: U32::new(kani::any()),
            segment_count: U32::new(kani::any()),
            file_type: kani::any(),
            kind: kani::any(),
            fill: kani::any(),
            digest_flags: kani::any(),
            sha256: kani::any(),
            sha3_256: kani::any(),
        }
    }

    fn any_segment() -> SegmentRecord {
        SegmentRecord {
            name_offset: U32::new(kani::any()),
            offset: U32::new(kani::any()),
            stored_size: U32::new(kani::any()),
            loaded_size: U32::new(kani::any()),
            in_place_size: U32::new(kani::any()),
            load_addr: U64::new(kani::any()),
            kind: kani::any(),
            compression: kani::any(),
            flags: kani::any(),
            _reserved: kani::any(),
            initialized_size: U32::new(kani::any()),
            stored_digest: kani::any(),
            loaded_digest: kani::any(),
        }
    }

    /// Any two-record tables over an ASCII string table that pass
    /// `validate_tables`, the first phase of `validate`: every later phase
    /// and every lookup runs only on such views. `validate` is the five
    /// phases in sequence, so each phase's harness below states a property of
    /// every validated view, and lookups are proven under the weaker first
    /// phase alone. Records are built field by field; the wire layout of
    /// parsed views is proven separately.
    fn with_tables(check: impl FnOnce(&DirectoryView<'_>)) {
        let regions: [RegionRecord; R] = [any_region(), any_region()];
        let entries: [EntryRecord; E] = [any_entry(), any_entry()];
        let segments: [SegmentRecord; S] = [any_segment(), any_segment()];
        let strings = any_string_table();
        let view = DirectoryView {
            regions: &regions,
            entries: &entries,
            segments: &segments,
            strings: &strings,
        };
        kani::assume(view.validate_tables().is_ok());
        check(&view);
    }

    /// Any query of at most one ASCII byte: spelled by the table or absent.
    fn with_query(check: impl FnOnce(&str)) {
        let bytes: [u8; 1] = [kani::any_where(u8::is_ascii)];
        let len: usize = kani::any_where(|&l| l <= 1);
        check(core::str::from_utf8(&bytes[..len]).unwrap());
    }

    /// A non-empty query that no name in the table can spell.
    fn absent(view: &DirectoryView<'_>, name: &str) -> bool {
        let [x, _, y, _] = view.strings else {
            return false;
        };
        name.len() == 1 && name.as_bytes()[0] != *x && name.as_bytes()[0] != *y
    }

    /// Count container-owned files whose index satisfies `matches`;
    /// remember the last (region, entry) pair.
    fn files_matching(
        view: &DirectoryView<'_>,
        matches: impl Fn(usize) -> bool,
    ) -> (usize, usize, usize) {
        let (mut count, mut hit_r, mut hit_e) = (0, 0, 0);
        for (r, region) in view.regions.iter().enumerate() {
            if region.kind != REGION_KIND_CONTAINER {
                continue;
            }
            let first = region.first_entry.get() as usize;
            let end = first + region.entry_count.get() as usize;
            for (e, entry) in view.entries.iter().enumerate() {
                if e >= first && e < end && entry.kind == ENTRY_KIND_FILE && matches(e) {
                    count += 1;
                    (hit_r, hit_e) = (r, e);
                }
            }
        }
        (count, hit_r, hit_e)
    }

    /// Each entry's name, decoded once.
    fn entry_names<'a>(view: &DirectoryView<'a>) -> [Result<&'a str, ReaderError>; E] {
        core::array::from_fn(|e| view.string(view.entries[e].name_offset.get()))
    }

    /// The file view is exactly entry `e` reached through region `r`: that
    /// region's offset, that record and that entry's segment slice.
    fn assert_file_is(view: &DirectoryView<'_>, file: &FileView<'_>, r: usize, e: usize) {
        let entry = &view.entries[e];
        assert!(core::ptr::eq(file.entry, entry));
        assert_eq!(file.region_offset(), view.regions[r].offset.get());
        assert_eq!(file.entry_offset(), entry.offset.get());
        let first = entry.first_segment.get() as usize;
        let count = entry.segment_count.get() as usize;
        assert!(core::ptr::eq(
            file.segments(),
            &view.segments[first..first + count]
        ));
        assert!(core::ptr::eq(file.strings, view.strings));
    }

    /// Found exactly when exactly one (container, file) pair has the type,
    /// and then it is that file; more are ambiguous; never a panic.
    #[kani::proof]
    #[kani::unwind(3)]
    #[kani::stub(core::str::from_utf8, from_utf8_ascii)]
    fn verify_directory_find_file_by_type_is_exact() {
        with_tables(|view| {
            let raw_type: u8 = kani::any_where(|&t| (1..=10).contains(&t));
            let file_type = decode_file_type(raw_type).unwrap();
            let (typed, r, e) = files_matching(view, |e| view.entries[e].file_type == raw_type);
            match view.find_file_by_type(file_type) {
                Ok(file) => {
                    kani::cover!(true, "file found by type");
                    assert_eq!(typed, 1);
                    assert_eq!(file.file_type(), Ok(file_type));
                    assert_file_is(view, &file, r, e);
                }
                Err(ReaderError::FileNotFound) => assert_eq!(typed, 0),
                Err(ReaderError::DeserializeError) => {
                    kani::cover!(true, "ambiguous type rejected");
                    assert!(typed >= 2);
                }
                Err(_) => panic!("validated tables have no other lookup error"),
            }
        });
    }

    /// Lookup by any short name, spelled by the table or absent from it:
    /// found exactly when one (container, file) pair has the name, and then
    /// it is that file.
    #[kani::proof]
    #[kani::unwind(3)]
    #[kani::stub(core::str::from_utf8, from_utf8_ascii)]
    fn verify_directory_find_file_by_name_is_exact() {
        with_tables(|view| {
            with_query(|name| {
                let names = entry_names(view);
                let (named, r, e) = files_matching(view, |e| names[e] == Ok(name));
                match view.find_file_by_name(name) {
                    Ok(file) => {
                        kani::cover!(
                            !name.is_empty()
                                && view.entries.iter().all(|e| e.kind == ENTRY_KIND_FILE)
                                && names[0] != names[1],
                            "file found among two distinct names"
                        );
                        assert_eq!(named, 1);
                        assert_eq!(file.name(), Ok(name));
                        assert_file_is(view, &file, r, e);
                    }
                    Err(ReaderError::FileNotFound) => {
                        kani::cover!(absent(view, name), "name absent from the string table");
                        assert_eq!(named, 0);
                    }
                    Err(ReaderError::DeserializeError) => {
                        kani::cover!(true, "ambiguous name rejected");
                        assert!(named >= 2);
                    }
                    Err(_) => panic!("validated tables have no other lookup error"),
                }
            });
        });
    }

    /// A raw-region lookup returns exactly the raw region of that name,
    /// rejects a container or a duplicate of that name, and reports an absent
    /// name as `None`.
    #[kani::proof]
    #[kani::unwind(3)]
    #[kani::stub(core::str::from_utf8, from_utf8_ascii)]
    fn verify_directory_raw_region_is_exact() {
        with_tables(|view| {
            with_query(|name| {
                let (mut named, mut hit) = (0, 0);
                for (r, region) in view.regions.iter().enumerate() {
                    if view.string(region.name_offset.get()) == Ok(name) {
                        named += 1;
                        hit = r;
                    }
                }
                let region = &view.regions[hit];
                let expected = match (named, region.kind) {
                    (0, _) => Ok(None),
                    (1, REGION_KIND_RAW) => {
                        Ok(Some((region.offset.get(), region.size.get(), region.fill)))
                    }
                    _ => Err(ReaderError::DeserializeError),
                };
                kani::cover!(matches!(expected, Ok(Some(_))), "raw region found");
                kani::cover!(
                    named == 1 && expected.is_err(),
                    "container of that name rejected"
                );
                kani::cover!(absent(view, name), "name absent from the string table");
                assert_eq!(view.raw_region(name), expected);
            });
        });
    }

    /// Region phase: region names are unique, each container's children have
    /// unique names, and every extent fits its parent without overflow.
    #[kani::proof]
    #[kani::unwind(3)]
    #[kani::stub(core::str::from_utf8, from_utf8_ascii)]
    fn verify_directory_validate_regions_enforces_unique_contained_names() {
        with_tables(|view| {
            if view.validate_regions().is_err() {
                return;
            }
            kani::cover!(
                view.regions.iter().all(|r| r.kind == REGION_KIND_CONTAINER),
                "two containers accepted"
            );
            let [a, b] = [&view.regions[0], &view.regions[1]];
            assert!(view.string(a.name_offset.get()) != view.string(b.name_offset.get()));
            let names = entry_names(view);
            for region in view.regions {
                assert!(region._reserved == [0; 2]);
                assert!(region.offset.get().checked_add(region.size.get()).is_some());
                if region.kind != REGION_KIND_CONTAINER {
                    continue;
                }
                let first = region.first_entry.get() as usize;
                let end = first + region.entry_count.get() as usize;
                for (i, entry) in view.entries.iter().enumerate() {
                    if i < first || i >= end {
                        continue;
                    }
                    assert!(
                        u64::from(entry.offset.get()) + u64::from(entry.size.get())
                            <= u64::from(region.size.get())
                    );
                    if i > first {
                        assert!(names[i - 1] != names[i]);
                    }
                }
            }
        });
    }

    /// File phase: every file has a SHA-256 digest only, no fill and at
    /// least one segment, each stored within the file.
    #[kani::proof]
    #[kani::unwind(3)]
    #[kani::stub(core::str::from_utf8, from_utf8_ascii)]
    fn verify_directory_validate_files_bounds_segments() {
        with_tables(|view| {
            if view.validate_files().is_err() {
                return;
            }
            for entry in view.entries {
                if entry.kind != ENTRY_KIND_FILE {
                    continue;
                }
                kani::cover!(entry.segment_count.get() == 2, "file with two segments");
                assert!(entry.segment_count.get() != 0 && entry.fill == 0);
                assert_eq!(entry.digest_flags, DIGEST_SHA256);
                assert!(entry.sha3_256 == [0; 32]);
                let first = entry.first_segment.get() as usize;
                let end = first + entry.segment_count.get() as usize;
                for (i, s) in view.segments.iter().enumerate() {
                    if i >= first && i < end {
                        assert!(
                            u64::from(s.offset()) + u64::from(s.stored_size())
                                <= u64::from(entry.size.get())
                        );
                    }
                }
            }
        });
    }

    /// Partition phase: every entry has exactly one container and every
    /// segment exactly one file, so a lookup can never reach a record twice.
    #[kani::proof]
    #[kani::unwind(3)]
    #[kani::stub(core::str::from_utf8, from_utf8_ascii)]
    fn verify_directory_validate_partition_gives_unique_owners() {
        with_tables(|view| {
            if view.validate_partition().is_err() {
                return;
            }
            kani::cover!(
                view.regions[0].kind == REGION_KIND_CONTAINER
                    && view.regions[0].entry_count.get() == 2,
                "one container owns both entries"
            );
            kani::cover!(
                view.entries.iter().any(|e| e.kind == ENTRY_KIND_RAW),
                "raw entry uniquely owned"
            );
            for e in 0..E {
                let mut owners = 0;
                for region in view.regions {
                    let first = region.first_entry.get() as usize;
                    let end = first + region.entry_count.get() as usize;
                    if region.kind == REGION_KIND_CONTAINER && e >= first && e < end {
                        owners += 1;
                    }
                }
                assert_eq!(owners, 1);
            }
            for s in 0..S {
                let mut owners = 0;
                for entry in view.entries {
                    let first = entry.first_segment.get() as usize;
                    let end = first + entry.segment_count.get() as usize;
                    if entry.kind == ENTRY_KIND_FILE && s >= first && s < end {
                        owners += 1;
                    }
                }
                assert_eq!(owners, 1);
            }
        });
    }

    /// Segment phase: decodable kinds and compression, named, initialized
    /// bytes within memory, no address overflow, stored segments carry their
    /// digests and uncompressed ones are their own initialized image.
    #[kani::proof]
    #[kani::unwind(3)]
    #[kani::stub(core::str::from_utf8, from_utf8_ascii)]
    fn verify_directory_validate_segments_enforces_segment_rules() {
        with_tables(|view| {
            if view.validate_segments().is_err() {
                return;
            }
            for s in view.segments {
                let kind = s.kind().unwrap();
                let compression = s.compression().unwrap();
                kani::cover!(compression == Compression::Lz4, "compressed segment");
                assert!(s.name(view.strings).is_ok());
                assert!(s.initialized_size() <= s.loaded_size());
                assert!(
                    s.load_addr()
                        .checked_add(u64::from(s.loaded_size()))
                        .is_some()
                );
                assert!(s._reserved == 0 && s.flags & !7 == 0);
                if s.stored_size() == 0 {
                    assert!(kind == SegmentKind::Bss && s.initialized_size() == 0);
                    assert!(compression == Compression::None);
                    assert!(s.stored_digest == [0; 32] && s.loaded_digest == [0; 32]);
                } else {
                    assert!(kind != SegmentKind::Bss && s.initialized_size() != 0);
                }
                if compression == Compression::None {
                    assert_eq!(s.in_place_size(), 0);
                    if s.stored_size() != 0 {
                        assert_eq!(s.stored_size(), s.initialized_size());
                        assert!(s.stored_digest == s.loaded_digest);
                    }
                } else {
                    assert!(
                        u64::from(s.in_place_size())
                            >= u64::from(s.initialized_size()) + u64::from(s.stored_size())
                    );
                }
            }
        });
    }
}
