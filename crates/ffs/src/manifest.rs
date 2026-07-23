//! Zero-copy firmware manifest table format.
//!
//! The builder owns ergonomic [`ImageManifest`](fstart_core::ffs::ImageManifest)
//! values, but the bytes written into firmware are flat tables. Runtime code can
//! verify the signed byte range and borrow typed record slices directly from RO
//! flash/pflash without deserializing a heapless object graph.

use crate::reader::ReaderError;
use fstart_core::ffs::{
    Compression, DigestSet, EntryContent, FileType, ImageManifest, Region, RegionContent,
    RegionEntry, Segment, SegmentFlags, SegmentKind, Signature, SignatureKind,
};
use heapless::String as HString;
use zerocopy::byteorder::{LE, U32, U64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Ref, Unaligned};

const MAGIC: u32 = u32::from_le_bytes(*b"FSMZ");
const VERSION: u32 = 1;
const SIGNED_MAGIC: u32 = u32::from_le_bytes(*b"FSSZ");
const SIGNED_VERSION: u32 = 1;

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
struct SignedHeader {
    magic: U32<LE>,
    version: U32<LE>,
    manifest_size: U32<LE>,
}

#[derive(Clone, Copy, Debug, FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned)]
#[repr(C)]
struct SignatureRecord {
    key_id: u8,
    kind: u8,
    _reserved: [u8; 2],
    sig_lo: [u8; 32],
    sig_hi: [u8; 32],
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
}

const REGION_KIND_CONTAINER: u8 = 1;
const REGION_KIND_RAW: u8 = 2;
const ENTRY_KIND_FILE: u8 = 1;
const ENTRY_KIND_RAW: u8 = 2;
const DIGEST_SHA256: u8 = 1 << 0;
const DIGEST_SHA3_256: u8 = 1 << 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ManifestSummary {
    pub regions: usize,
    pub entries: usize,
}

#[derive(Clone, Copy)]
pub struct ManifestView<'a> {
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

impl<'a> ManifestView<'a> {
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
        if strings_end > bytes.len() {
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
        for segment in self.segments {
            self.string(segment.name_offset.get())?;
            decode_segment_kind(segment.kind)?;
            decode_compression(segment.compression)?;
        }
        Ok(())
    }

    pub fn summary(&self) -> ManifestSummary {
        ManifestSummary {
            regions: self.regions.len(),
            entries: self.entries.len(),
        }
    }

    pub fn find_file_by_type(&self, file_type: FileType) -> Result<FileView<'a>, ReaderError> {
        for region in self.regions {
            if region.kind != REGION_KIND_CONTAINER {
                continue;
            }
            let first = region.first_entry.get() as usize;
            let count = region.entry_count.get() as usize;
            for entry in &self.entries[first..first + count] {
                if entry.kind == ENTRY_KIND_FILE && entry.file_type == encode_file_type(file_type) {
                    return self.file_view(region, entry);
                }
            }
        }
        Err(ReaderError::FileNotFound)
    }

    pub fn find_file_by_name(&self, name: &str) -> Result<FileView<'a>, ReaderError> {
        for region in self.regions {
            if region.kind != REGION_KIND_CONTAINER {
                continue;
            }
            let first = region.first_entry.get() as usize;
            let count = region.entry_count.get() as usize;
            for entry in &self.entries[first..first + count] {
                if entry.kind == ENTRY_KIND_FILE && self.string(entry.name_offset.get())? == name {
                    return self.file_view(region, entry);
                }
            }
        }
        Err(ReaderError::FileNotFound)
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
    Ok(out)
}

#[cfg(feature = "std")]
pub fn encode_signed_manifest(
    manifest_bytes: &[u8],
    signature: &Signature,
) -> Result<alloc::vec::Vec<u8>, alloc::string::String> {
    use alloc::string::ToString;
    use alloc::vec::Vec;

    let manifest_size = manifest_bytes
        .len()
        .try_into()
        .map_err(|_| "manifest too large".to_string())?;
    let header = SignedHeader {
        magic: U32::new(SIGNED_MAGIC),
        version: U32::new(SIGNED_VERSION),
        manifest_size: U32::new(manifest_size),
    };
    let signature = SignatureRecord::from_signature(signature);

    let mut out = Vec::with_capacity(
        core::mem::size_of::<SignedHeader>()
            + manifest_bytes.len()
            + core::mem::size_of::<SignatureRecord>(),
    );
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(manifest_bytes);
    out.extend_from_slice(signature.as_bytes());
    Ok(out)
}

pub fn parse_signed_manifest(data: &[u8]) -> Result<(&[u8], Signature), ReaderError> {
    let header_size = core::mem::size_of::<SignedHeader>();
    let signature_size = core::mem::size_of::<SignatureRecord>();
    let (header_ref, _) =
        Ref::<_, SignedHeader>::from_prefix(data).map_err(|_| ReaderError::DeserializeError)?;
    let header = *header_ref;
    if header.magic.get() != SIGNED_MAGIC || header.version.get() != SIGNED_VERSION {
        return Err(ReaderError::UnsupportedVersion);
    }

    let manifest_start = header_size;
    let manifest_end = manifest_start
        .checked_add(header.manifest_size.get() as usize)
        .ok_or(ReaderError::OutOfBounds)?;
    let signature_end = manifest_end
        .checked_add(signature_size)
        .ok_or(ReaderError::OutOfBounds)?;
    if signature_end != data.len() {
        return Err(ReaderError::DeserializeError);
    }
    let manifest = data
        .get(manifest_start..manifest_end)
        .ok_or(ReaderError::OutOfBounds)?;
    let signature_bytes = data
        .get(manifest_end..signature_end)
        .ok_or(ReaderError::OutOfBounds)?;
    let signature_ref = Ref::<_, SignatureRecord>::from_bytes(signature_bytes)
        .map_err(|_| ReaderError::DeserializeError)?;
    Ok((manifest, Ref::into_ref(signature_ref).to_signature()?))
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
    });
    Ok(())
}

#[cfg(feature = "std")]
fn push_string(strings: &mut alloc::vec::Vec<u8>, s: &str) -> Result<u32, alloc::string::String> {
    use alloc::string::ToString;
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

impl SignatureRecord {
    #[cfg(feature = "std")]
    fn from_signature(signature: &Signature) -> Self {
        Self {
            key_id: signature.key_id,
            kind: encode_signature_kind(signature.kind),
            _reserved: [0; 2],
            sig_lo: signature.sig_lo,
            sig_hi: signature.sig_hi,
        }
    }

    fn to_signature(self) -> Result<Signature, ReaderError> {
        Ok(Signature {
            key_id: self.key_id,
            kind: decode_signature_kind(self.kind)?,
            sig_lo: self.sig_lo,
            sig_hi: self.sig_hi,
        })
    }
}

#[cfg(feature = "std")]
fn encode_signature_kind(kind: SignatureKind) -> u8 {
    match kind {
        SignatureKind::Ed25519 => 1,
        SignatureKind::EcdsaP256 => 2,
    }
}

fn decode_signature_kind(value: u8) -> Result<SignatureKind, ReaderError> {
    match value {
        1 => Ok(SignatureKind::Ed25519),
        2 => Ok(SignatureKind::EcdsaP256),
        _ => Err(ReaderError::DeserializeError),
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
        _ => Err(ReaderError::DeserializeError),
    }
}
