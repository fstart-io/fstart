//! Build-generated coreboot payload asset: launch policy plus the files
//! handed to the payload.
//!
//! fbuild flattens the payload ELF into one FFS payload file and packs
//! everything else into the verified data asset [`ASSET`]: the entry point
//! from the ELF header, then the files given on the command line. One asset
//! keeps the FFS file count fixed however many files there are.
//!
//! Wire format, all little-endian: [`Header`], `file_count` [`FileEntry`]
//! records, then the file bytes each record points at.

use zerocopy::little_endian::{U32, U64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

/// Verified FFS asset holding the manifest and the files.
pub const ASSET: &str = "coreboot-payload";
/// Longest file name.
pub const MAX_NAME: usize = 56;
/// Most files one payload can be given.
pub const MAX_FILES: usize = 16;
/// Room for the coreboot table itself beside the files.
pub const TABLE_CAPACITY: usize = 0x4000;
/// Files are placed on this boundary when handed over.
pub const FILE_ALIGN: usize = 16;

const MAGIC: [u8; 8] = *b"FSCBP001";

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct Header {
    magic: [u8; 8],
    entry: U64,
    file_count: U32,
    reserved: U32,
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct FileEntry {
    /// From the start of the asset.
    offset: U32,
    size: U32,
    /// NUL-padded.
    name: [u8; MAX_NAME],
}

const HEADER_LEN: usize = core::mem::size_of::<Header>();
const ENTRY_LEN: usize = core::mem::size_of::<FileEntry>();

/// Decoded asset, borrowing its bytes.
#[derive(Clone, Copy)]
pub struct Manifest<'a> {
    /// 32-bit protected-mode entry point.
    pub entry: u64,
    entries: &'a [FileEntry],
    bytes: &'a [u8],
}

impl<'a> Manifest<'a> {
    #[must_use]
    pub fn decode(bytes: &'a [u8]) -> Option<Self> {
        let (header, rest) = Header::ref_from_prefix(bytes).ok()?;
        let count = header.file_count.get() as usize;
        if header.magic != MAGIC || header.reserved.get() != 0 || count > MAX_FILES {
            return None;
        }
        let (entries, _) = <[FileEntry]>::ref_from_prefix_with_elems(rest, count).ok()?;
        let manifest = Self {
            entry: header.entry.get(),
            entries,
            bytes,
        };
        let valid = entries.iter().all(|entry| {
            let start = entry.offset.get() as usize;
            !name(entry).is_empty()
                && start >= HEADER_LEN + count * ENTRY_LEN
                && start
                    .checked_add(entry.size.get() as usize)
                    .is_some_and(|end| end <= bytes.len())
        });
        valid.then_some(manifest)
    }

    /// The handed-over files: name and contents.
    pub fn files(&self) -> impl Iterator<Item = (&'a str, &'a [u8])> + 'a {
        let bytes = self.bytes;
        self.entries.iter().map(move |entry| {
            let start = entry.offset.get() as usize;
            (
                name(entry),
                &bytes[start..start + entry.size.get() as usize],
            )
        })
    }

    /// Bytes of RAM the files and the table need together.
    #[must_use]
    pub fn window_size(&self) -> usize {
        self.files()
            .map(|(_, data)| data.len().next_multiple_of(FILE_ALIGN))
            .sum::<usize>()
            + TABLE_CAPACITY
    }
}

fn name(entry: &FileEntry) -> &str {
    let len = entry.name.iter().position(|&b| b == 0).unwrap_or(MAX_NAME);
    core::str::from_utf8(&entry.name[..len]).unwrap_or("")
}

/// Encoded size of an asset holding `files`.
#[must_use]
pub fn encoded_len(files: &[(&str, &[u8])]) -> usize {
    HEADER_LEN + files.len() * ENTRY_LEN + files.iter().map(|(_, data)| data.len()).sum::<usize>()
}

/// Encode into `out`, which must be exactly [`encoded_len`] bytes.
pub fn encode(entry: u64, files: &[(&str, &[u8])], out: &mut [u8]) -> Result<(), &'static str> {
    if files.len() > MAX_FILES {
        return Err("too many coreboot payload files");
    }
    if out.len() != encoded_len(files) || out.len() > u32::MAX as usize {
        return Err("coreboot payload asset has the wrong size");
    }
    let header = Header {
        magic: MAGIC,
        entry: entry.into(),
        file_count: (files.len() as u32).into(),
        reserved: 0.into(),
    };
    out[..HEADER_LEN].copy_from_slice(header.as_bytes());
    let mut data_at = HEADER_LEN + files.len() * ENTRY_LEN;
    for (index, &(name, data)) in files.iter().enumerate() {
        if name.is_empty() || name.len() > MAX_NAME || name.contains('\0') {
            return Err("coreboot payload file name is empty, has a NUL or exceeds 56 bytes");
        }
        let mut entry = FileEntry {
            offset: (data_at as u32).into(),
            size: (data.len() as u32).into(),
            name: [0; MAX_NAME],
        };
        entry.name[..name.len()].copy_from_slice(name.as_bytes());
        let slot = HEADER_LEN + index * ENTRY_LEN;
        out[slot..slot + ENTRY_LEN].copy_from_slice(entry.as_bytes());
        out[data_at..data_at + data.len()].copy_from_slice(data);
        data_at += data.len();
    }
    Ok(())
}
