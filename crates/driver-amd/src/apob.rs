// SPDX-License-Identifier: MIT OR Apache-2.0
//! Bounded APOB reader (Picasso and Zeppelin).
//! Reads PSP-trained memory results; never retrains or guesses UMC register data.
use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned,
    byteorder::{LittleEndian, U16, U32, U64},
};

/// Raven/Picasso APOB capacity. Zeppelin's PSP copies up to 128 KiB.
pub const MAX_SIZE: usize = 0x10000;
pub const MAX_HOLES: usize = 18;
const SIGNATURE: u32 = 0x424f5041;

// Entry groups and types (AGESA APOB_GROUP and its types).
pub const GROUP_MEMORY: u32 = 1;
/// Memory: the general configuration (media, MEMCLK, populated channels).
pub const TYPE_MEM_GENERAL_CONFIG: u32 = 2;
/// Memory: the PIE power settings of the ABL.
pub const TYPE_MEM_PIE_PWR: u32 = 0xf;
pub const GROUP_FABRIC: u32 = 9;
/// Fabric: the system memory map.
pub const TYPE_FABRIC_MEMORY_MAP: u32 = 9;

#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct Base {
    signature: U32<LittleEndian>,
    version: U32<LittleEndian>,
    size: U32<LittleEndian>,
    first: U32<LittleEndian>,
}
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct Entry {
    group: U32<LittleEndian>,
    kind: U32<LittleEndian>,
    instance: U32<LittleEndian>,
    size: U32<LittleEndian>,
    hmac: [u8; 32],
}
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct MemoryConfig {
    media: U32<LittleEndian>,
    memclk: U16<LittleEndian>,
}
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct MapHeader {
    top: U64<LittleEndian>,
    count: U32<LittleEndian>,
    reserved: U32<LittleEndian>,
}
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct WireHole {
    base: U64<LittleEndian>,
    size: U64<LittleEndian>,
    kind: U32<LittleEndian>,
    reserved: U32<LittleEndian>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Header,
    Entry,
    MissingMap,
    Map,
    TooManyHoles,
    Overflow,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Hole {
    pub base: u64,
    pub size: u64,
    pub kind: u32,
}
impl Hole {
    const EMPTY: Self = Self {
        base: 0,
        size: 0,
        kind: 0,
    };
    pub fn end(&self) -> u64 {
        self.base + self.size
    } // checked during parsing
}
#[derive(Debug, Clone)]
pub struct MemoryMap {
    pub top: u64,
    holes: [Hole; MAX_HOLES],
    count: usize,
}
impl MemoryMap {
    pub fn holes(&self) -> &[Hole] {
        &self.holes[..self.count]
    }
    /// The UMA/MMIO boundary heuristic, kept in u64 so a 4-GiB
    /// ceiling is not accidentally truncated to zero. Runtime TOP_MEM also
    /// constrains this boundary before any allocation.
    pub fn low_top(&self) -> u64 {
        self.low_top_above(1 << 31)
    }
    /// The lowest hole below 4 GiB that ends above `floor` starts the
    /// MMIO/reserved region (Zeppelin uses a 1-GiB floor).
    pub fn low_top_above(&self, floor: u64) -> u64 {
        self.holes()
            .iter()
            .fold(self.top.min(1 << 32), |top, hole| {
                if hole.end() <= top && hole.end() > floor {
                    top.min(hole.base)
                } else {
                    top
                }
            })
    }
}

pub struct Apob<'a> {
    entries: &'a [u8],
    version: u32,
}
impl<'a> Apob<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        Self::parse_with_capacity(bytes, MAX_SIZE)
    }
    /// Parse an APOB that the PSP may have made up to `capacity` bytes long.
    pub fn parse_with_capacity(bytes: &'a [u8], capacity: usize) -> Result<Self, Error> {
        let (header, _) = Base::ref_from_prefix(bytes).map_err(|_| Error::Header)?;
        let size = header.size.get() as usize;
        let first = header.first.get() as usize;
        if header.signature.get() != SIGNATURE
            || size > capacity
            || size > bytes.len()
            || first < size_of::<Base>()
            || first > size
            || size - first < size_of::<Entry>()
        {
            return Err(Error::Header);
        }
        let result = Self {
            entries: &bytes[first..size],
            version: header.version.get(),
        };
        // Check the entire directory, including unknown groups, before using it.
        result.visit(|_, _, _, _| {})?;
        Ok(result)
    }
    pub fn version(&self) -> u32 {
        self.version
    }
    fn visit(&self, mut visitor: impl FnMut(u32, u32, u32, &'a [u8])) -> Result<(), Error> {
        let mut bytes = self.entries;
        while bytes.len() >= size_of::<Entry>() {
            let (entry, _) = Entry::ref_from_prefix(bytes).map_err(|_| Error::Entry)?;
            let size = entry.size.get() as usize;
            if size < size_of::<Entry>() || size > bytes.len() {
                return Err(Error::Entry);
            }
            visitor(
                entry.group.get(),
                entry.kind.get(),
                entry.instance.get(),
                &bytes[size_of::<Entry>()..size],
            );
            bytes = &bytes[size..];
        }
        Ok(())
    }
    fn find(&self, group: u32, kind: u32) -> Option<&'a [u8]> {
        self.find_instance(group, kind, None)
    }
    /// Payload (after the entry header) of the first entry of a group, type
    /// and optionally instance.
    pub fn find_instance(&self, group: u32, kind: u32, instance: Option<u32>) -> Option<&'a [u8]> {
        let mut found = None;
        self.visit(|g, k, i, payload| {
            if found.is_none() && (g, k) == (group, kind) && instance.is_none_or(|n| n == i) {
                found = Some(payload);
            }
        })
        .ok()?;
        found
    }
    /// Byte `offset` of the first entry of a group, type and optionally
    /// instance. Vendor offsets count from the entry header.
    pub fn byte(&self, group: u32, kind: u32, instance: Option<u32>, offset: usize) -> Option<u8> {
        self.find_instance(group, kind, instance)?
            .get(offset.checked_sub(size_of::<Entry>())?)
            .copied()
    }
    pub fn memory_clock_mhz(&self) -> Option<u16> {
        let config = self.find(GROUP_MEMORY, TYPE_MEM_GENERAL_CONFIG)?;
        let (config, _) = MemoryConfig::ref_from_prefix(config).ok()?;
        Some(config.memclk.get())
    }
    pub fn memory_map(&self) -> Result<MemoryMap, Error> {
        let bytes = self
            .find(GROUP_FABRIC, TYPE_FABRIC_MEMORY_MAP)
            .ok_or(Error::MissingMap)?;
        let (header, bytes) = MapHeader::ref_from_prefix(bytes).map_err(|_| Error::Map)?;
        let count = header.count.get() as usize;
        if count > MAX_HOLES {
            return Err(Error::TooManyHoles);
        }
        if header.top.get() == 0 || count > bytes.len() / size_of::<WireHole>() {
            return Err(Error::Map);
        }
        let mut holes = [Hole::EMPTY; MAX_HOLES];
        for (hole, wire) in holes[..count]
            .iter_mut()
            .zip(bytes.as_chunks::<{ size_of::<WireHole>() }>().0.iter())
        {
            let wire = WireHole::ref_from_bytes(wire).map_err(|_| Error::Map)?;
            *hole = Hole {
                base: wire.base.get(),
                size: wire.size.get(),
                kind: wire.kind.get(),
            };
            hole.base.checked_add(hole.size).ok_or(Error::Overflow)?;
        }
        // All later resource consumers receive deterministic address order.
        holes[..count].sort_unstable_by_key(|hole| hole.base);
        Ok(MemoryMap {
            top: header.top.get(),
            holes,
            count,
        })
    }
}
