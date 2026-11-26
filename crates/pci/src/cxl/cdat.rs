//! The Coherent Device Attribute Table (CDAT 1.04) of a CXL device: read
//! entry by entry through DOE Table Access (CXL 3.1 §8.1.11, vendor
//! [`super::VENDOR`], type 2) and parsed into its structures.
//!
//! The read sizes itself from the table's header ([`length`]), unlike the
//! vendors' fixed 0x160-byte parse buffer ("Allocation size is smaller").

use pci_types::ConfigRegionAccess;
use zerocopy::little_endian::{U16, U32, U64};
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

use super::doe::{self, Doe};
use fstart_core::services::Timer;

/// The DOE data object type of CXL Table Access.
pub const TABLE_ACCESS: u8 = 2;
/// The CDAT's table type in a Table Access request.
pub const TABLE_CDAT: u8 = 0;
/// The entry handle that ends a read.
pub const LAST: u16 = 0xffff;
/// Bytes of the CDAT header.
pub const HEADER: usize = 16;

/// Structure types (CDAT 1.04 Table 2).
pub mod kind {
    pub const DSMAS: u8 = 0;
    pub const DSLBIS: u8 = 1;
    pub const DSMSCIS: u8 = 2;
    pub const DSIS: u8 = 3;
    pub const DSEMTS: u8 = 4;
    pub const SSLBIS: u8 = 5;
}

/// DSLBIS / SSLBIS data types (the HMAT's).
pub mod data {
    pub const ACCESS_LATENCY: u8 = 0;
    pub const READ_LATENCY: u8 = 1;
    pub const WRITE_LATENCY: u8 = 2;
    pub const ACCESS_BANDWIDTH: u8 = 3;
    pub const READ_BANDWIDTH: u8 = 4;
    pub const WRITE_BANDWIDTH: u8 = 5;
}

/// Why the CDAT could not be read or parsed (AMD's codes: [`Error::code`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Doe(doe::Error),
    /// The device's mailbox has no CDAT protocol (`0x434C_0603`).
    Unsupported,
    /// The table needs this many bytes.
    Buffer(usize),
    /// A response or structure is malformed (`0x434C_07xx`).
    Malformed,
    /// The header's checksum does not sum the table to zero.
    Checksum,
}

impl From<doe::Error> for Error {
    fn from(e: doe::Error) -> Self {
        Self::Doe(e)
    }
}

impl Error {
    pub const fn code(self) -> u32 {
        match self {
            Self::Doe(e) => e.code(),
            Self::Unsupported => 0x434c_0603,
            Self::Buffer(_) => 0x434c_0701,
            Self::Malformed => 0x434c_0702,
            Self::Checksum => 0x434c_0703,
        }
    }
}

/// One Table Access read of entry `handle`: the entry's bytes into `out`
/// and the next handle.
fn entry<C: ConfigRegionAccess>(
    doe: &Doe<'_, C>,
    handle: u16,
    out: &mut [u32],
    timer: &dyn Timer,
) -> Result<(usize, u16), Error> {
    let [h1, h2] = doe::header(super::VENDOR, TABLE_ACCESS, 3);
    // Request code 0 (read entry), table type, entry handle.
    let request = [h1, h2, u32::from(handle) << 16 | u32::from(TABLE_CDAT) << 8];
    let mut scratch = [0u32; 3 + MAX_ENTRY_DWORDS];
    let got = doe.exchange(&request, &mut scratch, timer)?;
    // Response: two headers, the table access dword, the entry.
    if got < 3 || got > scratch.len() {
        return Err(Error::Malformed);
    }
    if scratch[0] & 0xff_ffff != (u32::from(TABLE_ACCESS) << 16 | u32::from(super::VENDOR)) {
        return Err(Error::Malformed);
    }
    let body = &scratch[3..got];
    if body.len() > out.len() {
        return Err(Error::Buffer(body.len() * 4));
    }
    out[..body.len()].copy_from_slice(body);
    Ok((body.len(), (scratch[2] >> 16) as u16))
}

/// Most dwords of one CDAT entry fstart reads (an SSLBIS of 64 port
/// pairs; the header and the other structures are a few dwords).
pub const MAX_ENTRY_DWORDS: usize = 4 + 2 * 64 + 4;

/// The CDAT's length in bytes, from its header (entry 0).
pub fn length<C: ConfigRegionAccess>(doe: &Doe<'_, C>, timer: &dyn Timer) -> Result<usize, Error> {
    let mut header = [0u32; HEADER / 4];
    let (n, _) = entry(doe, 0, &mut header, timer)?;
    if n * 4 < HEADER {
        return Err(Error::Malformed);
    }
    Ok(header[0] as usize)
}

/// Read the whole CDAT into `buf` (at least [`length`] bytes): the table's
/// bytes, header first, checksum checked.
pub fn read<'b, C: ConfigRegionAccess>(
    doe: &Doe<'_, C>,
    buf: &'b mut [u8],
    timer: &dyn Timer,
) -> Result<&'b [u8], Error> {
    let mut handle = 0u16;
    let mut at = 0usize;
    let mut dwords = [0u32; MAX_ENTRY_DWORDS];
    // A table has at most 0xFFFF entries.
    for _ in 0..=u16::MAX as usize {
        let (n, next) = entry(doe, handle, &mut dwords, timer)?;
        let bytes = n * 4;
        let end = at.checked_add(bytes).ok_or(Error::Malformed)?;
        if end > buf.len() {
            return Err(Error::Buffer(end));
        }
        for (chunk, dword) in buf[at..end]
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(&dwords[..n])
        {
            chunk.copy_from_slice(&dword.to_le_bytes());
        }
        at = end;
        if next == LAST {
            break;
        }
        handle = next;
    }
    let table = &buf[..at];
    let length = Cdat::new(table)?.length;
    if length > at {
        return Err(Error::Malformed);
    }
    let table = &table[..length];
    if table.iter().fold(0u8, |a, &b| a.wrapping_add(b)) != 0 {
        return Err(Error::Checksum);
    }
    Ok(table)
}

#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct RawHeader {
    length: U32,
    revision: u8,
    checksum: u8,
    _reserved: [u8; 6],
    sequence: U32,
}

#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct RawStructure {
    kind: u8,
    _reserved: u8,
    length: U16,
}

#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct RawDsmas {
    header: RawStructure,
    handle: u8,
    flags: u8,
    _reserved: U16,
    dpa_base: U64,
    dpa_length: U64,
}

#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct RawDslbis {
    header: RawStructure,
    handle: u8,
    flags: u8,
    data_type: u8,
    _reserved: u8,
    base_unit: U64,
    entries: [U16; 3],
    _reserved2: U16,
}

#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct RawDsmscis {
    header: RawStructure,
    handle: u8,
    _reserved: [u8; 3],
    cache_size: U64,
    attributes: U32,
}

#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct RawDsis {
    header: RawStructure,
    flags: u8,
    handle: u8,
    _reserved: U16,
}

#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct RawDsemts {
    header: RawStructure,
    handle: u8,
    memory_type: u8,
    _reserved: U16,
    dpa_offset: U64,
    dpa_length: U64,
}

#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct RawSslbis {
    header: RawStructure,
    data_type: u8,
    _reserved: [u8; 3],
    base_unit: U64,
}

/// A Device Scoped Memory Affinity Structure: one DPA range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dsmas {
    pub handle: u8,
    pub flags: u8,
    pub dpa_base: u64,
    pub dpa_length: u64,
}

/// A Device Scoped Latency and Bandwidth Information Structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dslbis {
    pub handle: u8,
    pub flags: u8,
    pub data_type: u8,
    pub base_unit: u64,
    pub entries: [u16; 3],
}

impl Dslbis {
    /// Entry 0 in units of the base unit (picoseconds for latencies,
    /// MB/s for bandwidths): the value the HMAT's SLLBI takes.
    pub fn value(&self) -> u64 {
        u64::from(self.entries[0]).saturating_mul(self.base_unit)
    }
}

/// A Device Scoped Memory Side Cache Information Structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dsmscis {
    pub handle: u8,
    pub cache_size: u64,
    pub attributes: u32,
}

/// A Device Scoped EFI Memory Type Structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dsemts {
    pub handle: u8,
    /// 0 conventional, 1 conventional specific-purpose, 2 reserved, 3
    /// soft-reserved... (CDAT 1.04 Table 7).
    pub memory_type: u8,
    pub dpa_offset: u64,
    pub dpa_length: u64,
}

/// A Switch Scoped Latency and Bandwidth Information Structure (its
/// entries are not decoded).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sslbis {
    pub data_type: u8,
    pub base_unit: u64,
}

/// One CDAT structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Structure {
    Dsmas(Dsmas),
    Dslbis(Dslbis),
    Dsmscis(Dsmscis),
    Dsis { flags: u8, handle: u8 },
    Dsemts(Dsemts),
    Sslbis(Sslbis),
    Other(u8),
}

/// A CDAT read into memory.
#[derive(Debug, Clone, Copy)]
pub struct Cdat<'a> {
    pub length: usize,
    pub revision: u8,
    pub sequence: u32,
    bytes: &'a [u8],
}

impl<'a> Cdat<'a> {
    /// The table in `bytes` (header first).
    pub fn new(bytes: &'a [u8]) -> Result<Self, Error> {
        let (header, _) = RawHeader::ref_from_prefix(bytes).map_err(|_| Error::Malformed)?;
        let length = header.length.get() as usize;
        if length < HEADER || length > bytes.len() {
            return Err(Error::Malformed);
        }
        let _ = header.checksum;
        Ok(Self {
            length,
            revision: header.revision,
            sequence: header.sequence.get(),
            bytes: &bytes[..length],
        })
    }

    /// Its structures in order; a malformed one ends the walk with an error.
    pub fn structures(&self) -> impl Iterator<Item = Result<Structure, Error>> + 'a {
        let bytes = self.bytes;
        let mut at = HEADER;
        core::iter::from_fn(move || {
            if at >= bytes.len() {
                return None;
            }
            let rest = &bytes[at..];
            let parsed = (|| {
                let (s, _) = RawStructure::ref_from_prefix(rest).map_err(|_| Error::Malformed)?;
                let length = usize::from(s.length.get());
                if length < 4 || length > rest.len() {
                    return Err(Error::Malformed);
                }
                let body = &rest[..length];
                macro_rules! raw {
                    ($t:ty) => {
                        <$t>::ref_from_prefix(body).map_err(|_| Error::Malformed)?.0
                    };
                }
                let structure = match s.kind {
                    kind::DSMAS => {
                        let r = raw!(RawDsmas);
                        Structure::Dsmas(Dsmas {
                            handle: r.handle,
                            flags: r.flags,
                            dpa_base: r.dpa_base.get(),
                            dpa_length: r.dpa_length.get(),
                        })
                    }
                    kind::DSLBIS => {
                        let r = raw!(RawDslbis);
                        Structure::Dslbis(Dslbis {
                            handle: r.handle,
                            flags: r.flags,
                            data_type: r.data_type,
                            base_unit: r.base_unit.get(),
                            entries: r.entries.map(|e| e.get()),
                        })
                    }
                    kind::DSMSCIS => {
                        let r = raw!(RawDsmscis);
                        Structure::Dsmscis(Dsmscis {
                            handle: r.handle,
                            cache_size: r.cache_size.get(),
                            attributes: r.attributes.get(),
                        })
                    }
                    kind::DSIS => {
                        let r = raw!(RawDsis);
                        Structure::Dsis {
                            flags: r.flags,
                            handle: r.handle,
                        }
                    }
                    kind::DSEMTS => {
                        let r = raw!(RawDsemts);
                        Structure::Dsemts(Dsemts {
                            handle: r.handle,
                            memory_type: r.memory_type,
                            dpa_offset: r.dpa_offset.get(),
                            dpa_length: r.dpa_length.get(),
                        })
                    }
                    kind::SSLBIS => {
                        let r = raw!(RawSslbis);
                        Structure::Sslbis(Sslbis {
                            data_type: r.data_type,
                            base_unit: r.base_unit.get(),
                        })
                    }
                    other => Structure::Other(other),
                };
                Ok((structure, length))
            })();
            match parsed {
                Ok((structure, length)) => {
                    at += length;
                    Some(Ok(structure))
                }
                Err(e) => {
                    at = bytes.len();
                    Some(Err(e))
                }
            }
        })
    }

    /// The DSMAS structures, in table order.
    pub fn dsmas(&self) -> impl Iterator<Item = Dsmas> + 'a {
        self.structures().filter_map(|s| match s {
            Ok(Structure::Dsmas(d)) => Some(d),
            _ => None,
        })
    }

    /// The DSLBIS structures, in table order.
    pub fn dslbis(&self) -> impl Iterator<Item = Dslbis> + 'a {
        self.structures().filter_map(|s| match s {
            Ok(Structure::Dslbis(d)) => Some(d),
            _ => None,
        })
    }

    /// The DSEMTS structures, in table order.
    pub fn dsemts(&self) -> impl Iterator<Item = Dsemts> + 'a {
        self.structures().filter_map(|s| match s {
            Ok(Structure::Dsemts(d)) => Some(d),
            _ => None,
        })
    }
}

#[cfg(test)]
pub(crate) mod testing {
    extern crate std;
    use std::vec::Vec;

    /// A CDAT with `structures` after its header, checksum fixed.
    pub fn table(structures: &[&[u8]]) -> Vec<u8> {
        let mut t = std::vec![0u8; super::HEADER];
        for s in structures {
            t.extend_from_slice(s);
        }
        let length = t.len() as u32;
        t[..4].copy_from_slice(&length.to_le_bytes());
        t[4] = 1;
        let sum = t.iter().fold(0u8, |a, &b| a.wrapping_add(b));
        t[5] = sum.wrapping_neg();
        t
    }

    pub fn dsmas(handle: u8, base: u64, length: u64) -> Vec<u8> {
        let mut s = std::vec![0u8, 0, 24, 0, handle, 0, 0, 0];
        s.extend_from_slice(&base.to_le_bytes());
        s.extend_from_slice(&length.to_le_bytes());
        s
    }

    pub fn dslbis(handle: u8, data_type: u8, unit: u64, entry: u16) -> Vec<u8> {
        let mut s = std::vec![1u8, 0, 24, 0, handle, 0, data_type, 0];
        s.extend_from_slice(&unit.to_le_bytes());
        s.extend_from_slice(&entry.to_le_bytes());
        s.extend_from_slice(&[0; 6]);
        s
    }

    pub fn dsemts(handle: u8, memory_type: u8, offset: u64, length: u64) -> Vec<u8> {
        let mut s = std::vec![4u8, 0, 24, 0, handle, memory_type, 0, 0];
        s.extend_from_slice(&offset.to_le_bytes());
        s.extend_from_slice(&length.to_le_bytes());
        s
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::testing::*;
    use super::*;
    use crate::PciAddress;
    use crate::cxl::doe::testing::Model;
    use fstart_core::services::timer::ManualTimer;
    use std::vec::Vec;

    #[test]
    fn the_cdat_reads_entry_by_entry_and_parses() {
        let dsmas0 = dsmas(0, 0, 0x4000_0000);
        let lat = dslbis(0, data::READ_LATENCY, 1000, 150);
        let semts = dsemts(0, 1, 0, 0x4000_0000);
        let table = table(&[&dsmas0, &lat, &semts]);
        // Entries: header, then each structure.
        let entries: Vec<Vec<u8>> = [
            table[..HEADER].to_vec(),
            dsmas0.clone(),
            lat.clone(),
            semts.clone(),
        ]
        .into();
        let model = Model::new(0x300, move |request: &[u32]| {
            assert_eq!(request[0], 0x0002_1e98);
            let handle = (request[2] >> 16) as usize;
            let next = if handle + 1 == entries.len() {
                0xffff
            } else {
                handle as u32 + 1
            };
            let body: Vec<u32> = entries[handle]
                .chunks(4)
                .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
                .collect();
            let mut out = std::vec![0x0002_1e98, 3 + body.len() as u32, next << 16];
            out.extend(body);
            out
        });
        let timer = ManualTimer::new();
        let doe = unsafe { Doe::new(&model, PciAddress::new(0, 2, 0, 0), 0x300) };
        assert_eq!(length(&doe, &timer), Ok(table.len()));
        let mut small = [0u8; 32];
        assert_eq!(read(&doe, &mut small, &timer), Err(Error::Buffer(40)));
        let mut buf = std::vec![0u8; table.len()];
        let read = read(&doe, &mut buf, &timer).unwrap();
        assert_eq!(read, &table[..]);
        let cdat = Cdat::new(read).unwrap();
        assert_eq!(cdat.revision, 1);
        assert_eq!(
            cdat.dsmas().collect::<Vec<_>>(),
            [Dsmas {
                handle: 0,
                flags: 0,
                dpa_base: 0,
                dpa_length: 0x4000_0000
            }]
        );
        let l = cdat.dslbis().next().unwrap();
        assert_eq!((l.data_type, l.value()), (data::READ_LATENCY, 150_000));
        assert_eq!(cdat.dsemts().next().unwrap().memory_type, 1);
        assert_eq!(cdat.structures().count(), 3);
        // A truncated structure ends the walk with an error.
        let mut bad = table.clone();
        bad[HEADER + 2] = 0xff;
        let cdat = Cdat::new(&bad).unwrap();
        assert_eq!(cdat.structures().last(), Some(Err(Error::Malformed)));
    }
}
