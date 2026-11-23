// SPDX-License-Identifier: MIT OR Apache-2.0
//! CPU-independent Intel update-record format, shared by packaging and loading.
//! Bounds/checksums are structural checks, NOT Intel signature authentication,
//! licensing, revision-floor policy or platform RNG approval.
use zerocopy::{
    FromBytes, Immutable, KnownLayout,
    byteorder::{LE, U32},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Truncated,
    Header,
    Size,
    Checksum,
    Extended,
}

#[derive(FromBytes, Immutable, KnownLayout)]
#[repr(C)]
struct Header {
    version: U32<LE>,
    revision: U32<LE>,
    date: U32<LE>,
    signature: U32<LE>,
    checksum: U32<LE>,
    loader: U32<LE>,
    flags: U32<LE>,
    data_size: U32<LE>,
    total_size: U32<LE>,
    /// Metadata size and minimum update revision in newer releases.
    reserved: [U32<LE>; 3],
}
const _: () = assert!(core::mem::size_of::<Header>() == 48);
fn word(bytes: &[u8]) -> u32 {
    U32::<LE>::read_from_bytes(bytes)
        .expect("validated four-byte field")
        .get()
}
fn checksum(bytes: &[u8]) -> u32 {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .fold(0u32, |sum, bytes| sum.wrapping_add(word(bytes)))
}

#[derive(Debug, Clone, Copy)]
pub struct Record<'a> {
    pub offset: usize,
    bytes: &'a [u8],
    revision: u32,
    signature: u32,
    flags: u32,
    base_size: usize,
}
impl<'a> Record<'a> {
    pub fn bytes(self) -> &'a [u8] {
        self.bytes
    }
    pub fn revision(self) -> u32 {
        self.revision
    }
    pub fn matches(self, signature: u32, flags: u32) -> bool {
        if self.signature == signature && self.flags & flags != 0 {
            return true;
        }
        let ext = &self.bytes[self.base_size..];
        if ext.is_empty() {
            return false;
        }
        let count = word(&ext[..4]) as usize;
        ext[20..20 + count * 12]
            .as_chunks::<12>()
            .0
            .iter()
            .any(|entry| word(&entry[..4]) == signature && word(&entry[4..8]) & flags != 0)
    }
}

pub struct Records<'a> {
    bytes: &'a [u8],
    offset: usize,
}
pub fn records(bytes: &[u8]) -> Records<'_> {
    Records { bytes, offset: 0 }
}
impl<'a> Iterator for Records<'a> {
    type Item = Result<Record<'a>, Error>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.bytes.is_empty() {
            return None;
        }
        let result = parse(self.bytes, self.offset);
        match &result {
            Ok(record) => {
                self.bytes = &self.bytes[record.bytes.len()..];
                self.offset += record.bytes.len();
            }
            Err(_) => self.bytes = &[],
        }
        Some(result)
    }
}
fn parse(bytes: &[u8], offset: usize) -> Result<Record<'_>, Error> {
    let (header, _) = Header::read_from_prefix(bytes).map_err(|_| Error::Truncated)?;
    if header.version.get() != 1
        || header.loader.get() != 1
        || matches!(header.revision.get(), 0 | u32::MAX)
        || header.flags.get() & !0xff != 0
    {
        return Err(Error::Header);
    }
    let data = if header.data_size.get() == 0 {
        2000
    } else {
        header.data_size.get() as usize
    };
    let total = if header.total_size.get() == 0 {
        2048
    } else {
        header.total_size.get() as usize
    };
    let base = data.checked_add(48).ok_or(Error::Size)?;
    if data % 4 != 0 || total % 1024 != 0 || base > total {
        return Err(Error::Size);
    }
    let bytes = bytes.get(..total).ok_or(Error::Truncated)?;
    if checksum(&bytes[..base]) != 0 {
        return Err(Error::Checksum);
    }
    let ext = &bytes[base..];
    if !ext.is_empty() {
        if ext.len() < 20 || ext[8..20].iter().any(|v| *v != 0) {
            return Err(Error::Extended);
        }
        let count = word(&ext[..4]) as usize;
        let end = count
            .checked_mul(12)
            .and_then(|v| v.checked_add(20))
            .ok_or(Error::Extended)?;
        if end > ext.len() || checksum(ext) != 0 {
            return Err(Error::Extended);
        }
        let original = header
            .signature
            .get()
            .wrapping_add(header.flags.get())
            .wrapping_add(header.checksum.get());
        for alias in ext[20..end].as_chunks::<12>().0 {
            if word(&alias[4..8]) & !0xff != 0 || checksum(alias) != original {
                return Err(Error::Extended);
            }
        }
    }
    Ok(Record {
        offset,
        bytes,
        revision: header.revision.get(),
        signature: header.signature.get(),
        flags: header.flags.get(),
        base_size: base,
    })
}
