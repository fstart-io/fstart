//! Stage-local build-layout wire format.
//!
//! This describes pre-link physical reservations, not actual ELF section sizes,
//! an image directory, a memory-discovery result or an authentication authority.
//! The host encoder lives in `fstart-image-build`; firmware borrows these bytes
//! directly from initialized read-only storage. No native struct is a wire ABI.
//!
//! Version 1 uses little-endian scalars:
//! - 16-byte header: magic `[u8; 4]`, version `u16`, encoded length `u16`,
//!   stage index `u16`, region count `u16`, reserved `[u8; 4]` (zero).
//! - Each 24-byte region: kind `u16`, reserved `[u8; 6]` (zero), physical base
//!   `u64`, size `u64`. Ranges are nonempty and half-open, with checked ends.
//!
//! A descriptor must contain 1..=32 regions. Singleton roles cannot repeat;
//! `Reserved` can repeat for platform-specific exclusions. Containment and
//! permitted overlap (e.g. an image inside flash, a stack inside writable RAM)
//! are the host resolver's responsibility. Actual RAM availability is checked
//! after memory discovery. The wire parser checks structure, not those policies.

pub const MAGIC: [u8; 4] = *b"FSLY";
pub const VERSION: u16 = 1;
pub const HEADER_LEN: usize = 16;
pub const REGION_LEN: usize = 24;
pub const MAX_REGIONS: usize = 32;
pub const MAX_ENCODED_LEN: usize = HEADER_LEN + MAX_REGIONS * REGION_LEN;

/// Roles refer to reserved physical capacity, never predicted file extents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum RegionKind {
    /// Initialized stage image reservation (RAM or ROM).
    Image = 1,
    /// Writable stage footprint, including BSS and any stack/heap subranges.
    Writable = 2,
    Stack = 3,
    Heap = 4,
    /// CPU-mapped flash window. Block-media offsets are not physical addresses.
    Flash = 5,
    /// Additional physical exclusion, such as a handoff buffer.
    Reserved = 6,
}

impl RegionKind {
    fn decode(value: u16) -> Result<Self, Error> {
        match value {
            1 => Ok(Self::Image),
            2 => Ok(Self::Writable),
            3 => Ok(Self::Stack),
            4 => Ok(Self::Heap),
            5 => Ok(Self::Flash),
            6 => Ok(Self::Reserved),
            _ => Err(Error::UnknownRegionKind),
        }
    }
}

/// Small scalar value read on demand; the full descriptor is never copied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub kind: RegionKind,
    pub base: u64,
    pub size: u64,
}

impl Region {
    pub const fn end(self) -> Option<u64> {
        self.base.checked_add(self.size)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidLength,
    BadMagic,
    UnsupportedVersion,
    InvalidCount,
    ReservedBits,
    UnknownRegionKind,
    InvalidRange,
    DuplicateRegionKind,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "invalid stage layout: {self:?}")
    }
}

/// Validated, allocation-free view into ROM or a host byte buffer.
#[derive(Debug, Clone, Copy)]
pub struct Layout<'a> {
    bytes: &'a [u8],
}

impl<'a> Layout<'a> {
    /// Parse exactly one descriptor, rejecting truncation and trailing bytes.
    /// Input alignment and the native target's endian/word size do not matter.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        if !(HEADER_LEN..=MAX_ENCODED_LEN).contains(&bytes.len()) {
            return Err(Error::InvalidLength);
        }
        if bytes[..4] != MAGIC {
            return Err(Error::BadMagic);
        }
        if u16_at(bytes, 4) != VERSION {
            return Err(Error::UnsupportedVersion);
        }
        if usize::from(u16_at(bytes, 6)) != bytes.len() {
            return Err(Error::InvalidLength);
        }
        let count = usize::from(u16_at(bytes, 10));
        if !(1..=MAX_REGIONS).contains(&count) {
            return Err(Error::InvalidCount);
        }
        if bytes.len() != HEADER_LEN + count * REGION_LEN {
            return Err(Error::InvalidLength);
        }
        if bytes[12..HEADER_LEN].iter().any(|&b| b != 0) {
            return Err(Error::ReservedBits);
        }

        let mut seen = 0u16;
        for record in bytes[HEADER_LEN..].chunks_exact(REGION_LEN) {
            let kind = RegionKind::decode(u16_at(record, 0))?;
            if record[2..8].iter().any(|&b| b != 0) {
                return Err(Error::ReservedBits);
            }
            let region = decode_region(record, kind);
            if region.size == 0 || region.end().is_none() {
                return Err(Error::InvalidRange);
            }
            if kind != RegionKind::Reserved {
                let bit = 1 << kind as u16;
                if seen & bit != 0 {
                    return Err(Error::DuplicateRegionKind);
                }
                seen |= bit;
            }
        }
        Ok(Self { bytes })
    }

    /// Zero-based identity within the resolved build's stage list.
    pub fn stage_index(self) -> u16 {
        u16_at(self.bytes, 8)
    }

    pub fn as_bytes(self) -> &'a [u8] {
        self.bytes
    }

    pub fn regions(self) -> impl ExactSizeIterator<Item = Region> + 'a {
        self.bytes[HEADER_LEN..]
            .chunks_exact(REGION_LEN)
            .map(|record| {
                // parse() validated every record; the borrowed bytes are immutable.
                let kind = RegionKind::decode(u16_at(record, 0)).unwrap();
                decode_region(record, kind)
            })
    }

    /// Find a singleton role, or the first `Reserved` entry. Use `regions()`
    /// when consuming all additional exclusions.
    pub fn region(self, kind: RegionKind) -> Option<Region> {
        self.regions().find(|region| region.kind == kind)
    }
}

fn decode_region(record: &[u8], kind: RegionKind) -> Region {
    Region {
        kind,
        base: u64_at(record, 8),
        size: u64_at(record, 16),
    }
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Independently hand-written wire bytes, not produced by the host encoder.
    const FIXTURE: [u8; 40] = [
        b'F', b'S', b'L', b'Y', 1, 0, 40, 0, 7, 0, 1, 0, 0, 0, 0, 0, 5, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0x20, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0,
    ];

    #[test]
    fn decodes_unaligned_little_endian_bytes_without_copying() {
        let mut storage = [0u8; FIXTURE.len() + 1];
        storage[1..].copy_from_slice(&FIXTURE);
        let bytes = &storage[1..];
        let layout = Layout::parse(bytes).unwrap();
        assert_eq!(layout.as_bytes().as_ptr(), bytes.as_ptr());
        assert_eq!(layout.stage_index(), 7);
        assert_eq!(layout.regions().len(), 1);
        let flash = layout.region(RegionKind::Flash).unwrap();
        assert_eq!(flash.base, 0x2000_0000);
        assert_eq!(flash.size, 0x0200_0000);
        assert_eq!(flash.end(), Some(0x2200_0000));
        assert!(layout.region(RegionKind::Stack).is_none());
    }

    #[test]
    fn rejects_every_truncation_and_trailing_bytes() {
        for len in 0..FIXTURE.len() {
            assert!(Layout::parse(&FIXTURE[..len]).is_err(), "length {len}");
        }
        let mut trailing = [0; FIXTURE.len() + 1];
        trailing[..FIXTURE.len()].copy_from_slice(&FIXTURE);
        assert_eq!(Layout::parse(&trailing).unwrap_err(), Error::InvalidLength);
    }

    #[test]
    fn rejects_bad_header_records_and_ranges() {
        for (offset, value, error) in [
            (0, 0, Error::BadMagic),
            (4, 2, Error::UnsupportedVersion),
            (6, 39, Error::InvalidLength),
            (10, 0, Error::InvalidCount),
            (10, 33, Error::InvalidCount),
            (10, 2, Error::InvalidLength),
            (12, 1, Error::ReservedBits),
            (16, 7, Error::UnknownRegionKind),
            (18, 1, Error::ReservedBits),
            (35, 0, Error::InvalidRange),
        ] {
            let mut bytes = FIXTURE;
            bytes[offset] = value;
            assert_eq!(Layout::parse(&bytes).unwrap_err(), error, "offset {offset}");
        }
        let mut overflow = FIXTURE;
        overflow[24..32].copy_from_slice(&u64::MAX.to_le_bytes());
        assert_eq!(Layout::parse(&overflow).unwrap_err(), Error::InvalidRange);
    }
}
