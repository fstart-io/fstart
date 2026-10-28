// SPDX-License-Identifier: MIT OR Apache-2.0
//! APOB: the AMD PSP output block the ABL leaves in DRAM (PSP_APOB §3.5),
//! the same container on every Zen SoC. It holds PSP-trained memory
//! results; nothing here retrains or guesses UMC register data.
//!
//! One APOB per system at the BIOS directory entry 0x61 destination
//! (`0x3000_0000` on every SP3 donor); per-socket/die data are instances
//! inside it. Header `{'APOB', version, size, first entry}`, entries
//! `{group, type, instance, size}` + 32-byte HMAC = 0x30 bytes, `size`
//! including the header. Record offsets in the submodules are from the entry
//! start, as in the report.
//!
//! Generation rules (decompiler-only, AmdPspPeiV2[Ssp] `ffe1813a`):
//! ZP (Naples, Zeppelin): `version < 5` → first entry at `0x220`, else
//! `+0xc`; Rome/Milan: `version <= 4` is rejected; Raven/Picasso: `+0xc`.
//!
//! - [`sysmap`]: 9/9 system memory map and its conversion to a memory map
//! - [`ccx`]: 3/3 CCD/CCX/core map (Rome, Milan)
//! - [`nps`]: 9/0x13 NUMA nodes per socket, 9/0x14 extra domains
//! - [`phy`]: Milan 9/0x15 optional global DXIO PHY overrides
//! - [`mem`]: 1/2, 1/0xf, 1/0x10, 1/0x11 SPD, 8/8 SMBIOS inputs
//! - [`event`]: 7/6 ABL event log

pub mod ccx;
pub mod event;
pub mod mem;
pub mod nps;
pub mod phy;
pub mod sysmap;

pub use sysmap::{Hole, MemoryMap};

use zerocopy::little_endian::U32;
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

use crate::psp::Variant;

/// APOB group ids used by x86.
pub mod group {
    pub const MEMORY: u32 = 1;
    pub const CCX: u32 = 3;
    pub const NBIO: u32 = 4;
    pub const GENERAL: u32 = 7;
    pub const SMBIOS: u32 = 8;
    pub const FABRIC: u32 = 9;
}

/// Header signature.
pub const SIGNATURE: [u8; 4] = *b"APOB";
/// Entry header size (0x10 + 32-byte HMAC).
pub const ENTRY_HEADER_SIZE: usize = 0x30;
/// Naples: first entry of an APOB with `version < 5`.
pub const NAPLES_LEGACY_FIRST_ENTRY: usize = 0x220;
/// fstart's sanity bound for a header read from DRAM (AGESA has none); the
/// noncar SoCs bound it by their APOB region.
pub const MAX_SIZE: u32 = 16 << 20;

/// Instance of per-socket/die data: `(socket << 8) | die`; Rome and Milan
/// always use die 0.
pub const fn instance(socket: u8, die: u8) -> u32 {
    (socket as u32) << 8 | die as u32
}

#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct Header {
    signature: [u8; 4],
    version: U32,
    size: U32,
    first_entry: U32,
}

/// An entry header.
#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
pub struct EntryHeader {
    pub group: U32,
    pub kind: U32,
    pub instance: U32,
    /// Entry size including this header.
    pub size: U32,
    pub hmac: [u8; 32],
}

/// APOB errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApobError {
    /// No `'APOB'` signature.
    Signature,
    /// Version not accepted by this PSP's rule.
    Version(u32),
    /// Header size or first-entry offset outside the buffer.
    Size { size: u32, first: u32 },
    /// Malformed entry header at `offset`.
    Entry { offset: usize },
    /// No entry with that group, type and instance.
    NotFound {
        group: u32,
        kind: u32,
        instance: u32,
    },
    /// More than one entry with that group, type and instance.
    Duplicate {
        group: u32,
        kind: u32,
        instance: u32,
    },
    /// The entry is shorter than its record layout.
    TooShort { group: u32, kind: u32 },
    /// The record is not defined for this PSP.
    Unsupported {
        group: u32,
        kind: u32,
        variant: Variant,
    },
    /// A record field holds a value the decoder does not know.
    Value { group: u32, kind: u32, value: u32 },
}

/// A validated APOB.
#[derive(Clone, Copy)]
pub struct Apob<'a> {
    data: &'a [u8],
    variant: Variant,
    version: u32,
    first: usize,
}

impl<'a> Apob<'a> {
    /// Parse the APOB in `data` (at least `size` bytes as the header says)
    /// and check every entry header, unknown groups included.
    pub fn new(data: &'a [u8], variant: Variant) -> Result<Self, ApobError> {
        let (header, _) = Header::ref_from_prefix(data).map_err(|_| ApobError::Signature)?;
        if header.signature != SIGNATURE {
            return Err(ApobError::Signature);
        }
        let version = header.version.get();
        let (size, first) = (header.size.get(), header.first_entry.get());
        let first_entry = match variant {
            Variant::Zp if version < 5 => NAPLES_LEGACY_FIRST_ENTRY as u32,
            Variant::Ssp | Variant::Gn if version <= 4 => {
                return Err(ApobError::Version(version));
            }
            Variant::Zp | Variant::Rv | Variant::Ssp | Variant::Gn => first,
        };
        let size_error = ApobError::Size { size, first };
        let data = data.get(..size as usize).ok_or(size_error)?;
        if (first_entry as usize) < size_of::<Header>()
            || (size as usize).saturating_sub(first_entry as usize) < ENTRY_HEADER_SIZE
        {
            return Err(size_error);
        }
        let apob = Self {
            data,
            variant,
            version,
            first: first_entry as usize,
        };
        apob.entries().try_for_each(|entry| entry.map(drop))?;
        Ok(apob)
    }

    /// The APOB at physical address `address` (identity mapped).
    ///
    /// # Safety
    /// `address` holds an APOB written by the PSP and stays unchanged and
    /// unaliased by writes for `'static`.
    pub unsafe fn from_address(address: u64, variant: Variant) -> Result<Apob<'static>, ApobError> {
        let ptr = address as usize as *const u8;
        // SAFETY: the caller's contract covers the header.
        let header = unsafe { core::slice::from_raw_parts(ptr, size_of::<Header>()) };
        let (header, _) = Header::ref_from_prefix(header).map_err(|_| ApobError::Signature)?;
        let size = header.size.get();
        if header.signature != SIGNATURE {
            return Err(ApobError::Signature);
        }
        if size > MAX_SIZE {
            return Err(ApobError::Size {
                size,
                first: header.first_entry.get(),
            });
        }
        // SAFETY: as above, for the size the header gives.
        Apob::new(
            unsafe { core::slice::from_raw_parts(ptr, size as usize) },
            variant,
        )
    }

    pub fn variant(&self) -> Variant {
        self.variant
    }

    pub fn version(&self) -> u32 {
        self.version
    }

    /// Size from the header.
    pub fn size(&self) -> usize {
        self.data.len()
    }

    /// The DRAM to keep reserved: `roundup(size, 4 KiB)` (AGESA's memory
    /// allocation HOB; ACPI NVS on Rome/Milan, boot services data on Naples).
    pub fn reserved_size(&self) -> u64 {
        (self.data.len() as u64).next_multiple_of(0x1000)
    }

    /// All entries in order; stops after the first malformed one.
    pub fn entries(&self) -> Entries<'a> {
        Entries {
            data: self.data,
            offset: Some(self.first),
        }
    }

    /// The unique entry `(group, kind, instance)`. AGESA collects the
    /// group/type matches and requires exactly one with the instance.
    pub fn find(&self, group: u32, kind: u32, instance: u32) -> Result<Entry<'a>, ApobError> {
        let mut found = None;
        for entry in self.entries() {
            let entry = entry?;
            if (entry.group(), entry.kind(), entry.instance()) == (group, kind, instance) {
                if found.is_some() {
                    return Err(ApobError::Duplicate {
                        group,
                        kind,
                        instance,
                    });
                }
                found = Some(entry);
            }
        }
        found.ok_or(ApobError::NotFound {
            group,
            kind,
            instance,
        })
    }

    /// The first entry of `(group, kind)`, of any instance.
    pub fn first(&self, group: u32, kind: u32) -> Result<Entry<'a>, ApobError> {
        self.find_all(group, kind)
            .next()
            .unwrap_or(Err(ApobError::NotFound {
                group,
                kind,
                instance: 0,
            }))
    }

    /// Every entry of `(group, kind)`, any instance.
    pub fn find_all(
        &self,
        group: u32,
        kind: u32,
    ) -> impl Iterator<Item = Result<Entry<'a>, ApobError>> + use<'a> {
        self.entries().filter(move |e| {
            e.as_ref()
                .map_or(true, |e| (e.group(), e.kind()) == (group, kind))
        })
    }

    /// Fail unless the record exists on this PSP.
    pub(crate) fn require(
        &self,
        group: u32,
        kind: u32,
        variants: &[Variant],
    ) -> Result<(), ApobError> {
        if variants.contains(&self.variant) {
            Ok(())
        } else {
            Err(ApobError::Unsupported {
                group,
                kind,
                variant: self.variant,
            })
        }
    }
}

/// Iterator over the entries.
pub struct Entries<'a> {
    data: &'a [u8],
    offset: Option<usize>,
}

impl<'a> Iterator for Entries<'a> {
    type Item = Result<Entry<'a>, ApobError>;

    fn next(&mut self) -> Option<Self::Item> {
        // A tail shorter than an entry header is padding.
        let offset = self
            .offset
            .filter(|&o| self.data.len().saturating_sub(o) >= ENTRY_HEADER_SIZE)?;
        let entry = (|| {
            let bytes = &self.data[offset..];
            let (header, _) = EntryHeader::ref_from_prefix(bytes).ok()?;
            let size = header.size.get() as usize;
            let bytes = bytes.get(..size).filter(|_| size >= ENTRY_HEADER_SIZE)?;
            Some(Entry { header, bytes })
        })();
        self.offset = entry.as_ref().map(|e| offset + e.bytes.len());
        Some(entry.ok_or(ApobError::Entry { offset }))
    }
}

/// One entry.
#[derive(Clone, Copy)]
pub struct Entry<'a> {
    header: &'a EntryHeader,
    bytes: &'a [u8],
}

impl<'a> Entry<'a> {
    pub fn group(&self) -> u32 {
        self.header.group.get()
    }

    pub fn kind(&self) -> u32 {
        self.header.kind.get()
    }

    pub fn instance(&self) -> u32 {
        self.header.instance.get()
    }

    /// The whole entry, header included (report offsets index this).
    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    fn too_short(&self) -> ApobError {
        ApobError::TooShort {
            group: self.group(),
            kind: self.kind(),
        }
    }

    /// The record `T` at entry offset `offset`.
    pub(crate) fn at<T: FromBytes + KnownLayout + Immutable + Unaligned>(
        &self,
        offset: usize,
    ) -> Result<&'a T, ApobError> {
        self.bytes
            .get(offset..)
            .and_then(|b| T::ref_from_prefix(b).ok())
            .map(|(t, _)| t)
            .ok_or(self.too_short())
    }

    /// `count` records `T` at entry offset `offset`.
    pub(crate) fn slice<T: FromBytes + Immutable + Unaligned>(
        &self,
        offset: usize,
        count: usize,
    ) -> Result<&'a [T], ApobError> {
        self.bytes
            .get(offset..)
            .and_then(|b| <[T]>::ref_from_prefix_with_elems(b, count).ok())
            .map(|(t, _)| t)
            .ok_or(self.too_short())
    }

    /// The byte at entry offset `offset`.
    pub fn byte(&self, offset: usize) -> Result<u8, ApobError> {
        self.bytes.get(offset).copied().ok_or(self.too_short())
    }
}

#[cfg(test)]
pub(crate) mod build {
    //! APOB test vectors built from the recovered layouts (no runtime APOB
    //! dump exists, PSP_APOB §7).

    pub struct Builder {
        version: u32,
        first: u32,
        entries: Vec<u8>,
    }

    impl Builder {
        pub fn new(version: u32) -> Self {
            Self {
                version,
                first: 0x10,
                entries: Vec::new(),
            }
        }

        /// Pad the header to put the first entry at `first`.
        pub fn first_entry(mut self, first: u32) -> Self {
            self.first = first;
            self
        }

        /// Add an entry; `body` starts at entry offset 0x30.
        pub fn entry(mut self, group: u32, kind: u32, instance: u32, body: &[u8]) -> Self {
            let size = 0x30 + body.len() as u32;
            [group, kind, instance, size]
                .iter()
                .for_each(|w| self.entries.extend(w.to_le_bytes()));
            self.entries.extend([0xa5; 32]);
            self.entries.extend(body);
            self
        }

        pub fn build(self) -> Vec<u8> {
            let size = self.first + self.entries.len() as u32;
            let mut out = b"APOB".to_vec();
            [self.version, size, self.first]
                .iter()
                .for_each(|w| out.extend(w.to_le_bytes()));
            out.resize(self.first as usize, 0);
            out.extend(self.entries);
            out
        }
    }

    /// Little-endian body builder.
    #[derive(Default)]
    pub struct Body(pub Vec<u8>);

    impl Body {
        /// Place `bytes` at entry offset `offset` (>= 0x30).
        pub fn at(mut self, offset: usize, bytes: &[u8]) -> Self {
            let o = offset - 0x30;
            if self.0.len() < o + bytes.len() {
                self.0.resize(o + bytes.len(), 0);
            }
            self.0[o..o + bytes.len()].copy_from_slice(bytes);
            self
        }

        pub fn len(mut self, size: usize) -> Self {
            self.0.resize(size - 0x30, 0);
            self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::build::Builder;
    use super::*;

    #[test]
    fn version_rules() {
        let v4 = Builder::new(4)
            .first_entry(0x220)
            .entry(1, 2, 0, &[])
            .build();
        assert_eq!(
            Apob::new(&v4, Variant::Ssp).err(),
            Some(ApobError::Version(4))
        );
        assert_eq!(
            Apob::new(&v4, Variant::Gn).err(),
            Some(ApobError::Version(4))
        );
        // ZP < 5 ignores the header's first-entry offset, Raven follows it.
        let mut legacy = v4.clone();
        legacy[0xc..0x10].copy_from_slice(&0x10u32.to_le_bytes());
        let apob = Apob::new(&legacy, Variant::Zp).unwrap();
        assert_eq!(apob.find(1, 2, 0).map(|e| e.instance()), Ok(0));
        assert!(Apob::new(&legacy, Variant::Rv).is_err());
        let v2 = Builder::new(2).entry(1, 2, 0, &[]).build();
        assert_eq!(
            Apob::new(&v2, Variant::Rv)
                .unwrap()
                .first(1, 2)
                .map(|e| e.kind()),
            Ok(2)
        );
        let v5 = Builder::new(5).entry(1, 2, 0, &[]).build();
        assert!(Apob::new(&v5, Variant::Ssp).is_ok());
    }

    #[test]
    fn unique_instance() {
        let data = Builder::new(0x18)
            .entry(1, 0x11, instance(0, 0), &[0; 4])
            .entry(1, 0x11, instance(1, 0), &[1; 4])
            .entry(7, 6, 0, &[])
            .entry(7, 6, 0, &[])
            .build();
        let apob = Apob::new(&data, Variant::Gn).unwrap();
        assert_eq!(apob.find(1, 0x11, 0x100).unwrap().bytes()[0x30], 1);
        assert_eq!(apob.first(1, 0x11).unwrap().instance(), 0);
        assert_eq!(
            apob.find(7, 6, 0).err(),
            Some(ApobError::Duplicate {
                group: 7,
                kind: 6,
                instance: 0
            })
        );
        assert_eq!(
            apob.find(9, 9, 0).err(),
            Some(ApobError::NotFound {
                group: 9,
                kind: 9,
                instance: 0
            })
        );
        assert_eq!(apob.find_all(1, 0x11).count(), 2);
        assert_eq!(apob.reserved_size(), 0x1000);
    }

    #[test]
    fn malformed_entries_are_rejected() {
        let mut data = Builder::new(0x18)
            .entry(1, 2, 0, &[])
            .entry(9, 9, 0, &[])
            .build();
        // Second entry claims a size below the header size.
        data[0x40 + 0xc..0x40 + 0x10].copy_from_slice(&0x10u32.to_le_bytes());
        assert_eq!(
            Apob::new(&data, Variant::Ssp).err(),
            Some(ApobError::Entry { offset: 0x40 })
        );
        // A tail shorter than an entry header is padding; an APOB without
        // room for one entry is not an APOB.
        let mut padded = Builder::new(0x18).entry(1, 2, 0, &[]).build();
        padded.extend([0; 0x10]);
        let size = padded.len() as u32;
        padded[8..0xc].copy_from_slice(&size.to_le_bytes());
        assert!(Apob::new(&padded, Variant::Rv).is_ok_and(|apob| apob.first(1, 2).is_ok()));
        let empty = Builder::new(0x18).build();
        assert!(matches!(
            Apob::new(&empty, Variant::Rv),
            Err(ApobError::Size { .. })
        ));
        // Size larger than the buffer.
        assert!(matches!(
            Apob::new(&data[..0x50], Variant::Ssp),
            Err(ApobError::Size { .. })
        ));
    }
}
