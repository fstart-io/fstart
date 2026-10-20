//! APOB 9/9: the system memory map (PSP_APOB §3.5, §3.6).
//!
//! Instance 0: top of DRAM (`+0x30`, u64), hole count (`+0x38`) and holes
//! at `+0x40`, 0x18 bytes each `{base, size, type}`. Same layout on all
//! three generations. The x86 side never trains or maps DRAM; this list is
//! all it gets.
//!
//! [`memory_map`] turns it into RAM and reserved ranges. It follows AGESA's
//! memory-info HOB (ZP V1 `AmdMemoryHobInfoPeim`, SSP/GN V2
//! `AmdMemoryHobInfoPeimV2` `ffd4cdff`, instruction-checked) with these
//! deliberate differences:
//!
//! - holes are sorted and must not overlap (AGESA trusts APOB order);
//! - unknown hole types are reserved (AGESA skips them, which would turn
//!   the hole into RAM);
//! - the 12 GiB below 1 TiB (`[0xFD_0000_0000, 1 TiB)`, the HyperTransport
//!   region) is reserved whenever DRAM reaches it, on every generation.
//!   AGESA (Rome/Milan) only does so on a reserved hole above 1 TiB or a
//!   type-3 hole; AMI MemDetect reserves 4 GiB there if TOM2 is above it;
//! - more than [`MAX_HOLES`] holes is an error (Oxide's Milan cap).

use heapless::Vec;
use zerocopy::little_endian::{U32, U64};
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

use super::{Apob, ApobError, group};
use crate::soc::Generation;

/// Most holes accepted (Oxide's Milan cap; AGESA has none).
pub const MAX_HOLES: usize = 18;
/// Most regions [`memory_map`] returns.
pub const MAX_REGIONS: usize = 2 * (2 * MAX_HOLES + 1) + 1;
/// Start of the reserved region below 1 TiB.
pub const HT_HOLE_BASE: u64 = 0xfd_0000_0000;
/// 1 TiB, the end of that region.
pub const ONE_TIB: u64 = 1 << 40;

const KIND: u32 = 9;

#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct RawHead {
    top: U64,
    count: U32,
    _pad: U32,
}

/// A hole as the APOB stores it.
#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
pub struct RawHole {
    base: U64,
    size: U64,
    kind: U32,
    _pad: U32,
}

/// A hole in DRAM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hole {
    pub base: u64,
    pub size: u64,
    /// Raw type; see [`classify`].
    pub kind: u32,
}

impl From<&RawHole> for Hole {
    fn from(raw: &RawHole) -> Self {
        Self {
            base: raw.base.get(),
            size: raw.size.get(),
            kind: raw.kind.get(),
        }
    }
}

/// The 9/9 record.
pub struct SystemMemoryMap<'a> {
    /// Top of DRAM (exclusive).
    pub top: u64,
    raw: &'a [RawHole],
}

impl SystemMemoryMap<'_> {
    pub fn holes(&self) -> impl Iterator<Item = Hole> + '_ {
        self.raw.iter().map(Hole::from)
    }
}

/// Read 9/9 instance 0.
pub fn system_memory_map<'a>(apob: &Apob<'a>) -> Result<SystemMemoryMap<'a>, ApobError> {
    let entry = apob.find(group::FABRIC, KIND, 0)?;
    let head: &RawHead = entry.at(0x30)?;
    let count = head.count.get();
    if count as usize > MAX_HOLES {
        return Err(ApobError::Value {
            group: group::FABRIC,
            kind: KIND,
            value: count,
        });
    }
    Ok(SystemMemoryMap {
        top: head.top.get(),
        raw: entry.slice(0x40, count as usize)?,
    })
}

/// How AGESA treats a hole type (the per-generation dispatch,
/// `ffde8b3e` Rome, `ffd4ceed` Milan).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoleClass {
    /// 0.
    Uma,
    /// 1.
    Mmio,
    /// Reserved DRAM: 2 on all; Rome 4/5, Milan 6/7 (openSIL's Genoa
    /// numbering names 6 DRTM and 7 CVIP; Rome's 4/5 are unnamed).
    Reserved,
    /// 3: MMIO up to `0xFD_0000_0000`, then the 1 TiB remap region.
    Remap1T,
    /// Not handled by AGESA for this generation (skipped there, reserved
    /// here).
    Unknown,
}

pub const fn classify(generation: Generation, kind: u32) -> HoleClass {
    match (generation, kind) {
        (_, 0) => HoleClass::Uma,
        (_, 1) => HoleClass::Mmio,
        (_, 2) | (Generation::Rome, 4 | 5) | (Generation::Milan, 6 | 7) => HoleClass::Reserved,
        (Generation::Rome | Generation::Milan, 3) => HoleClass::Remap1T,
        _ => HoleClass::Unknown,
    }
}

/// What a region of the physical address space is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionKind {
    Ram,
    Uma,
    Mmio,
    /// A reserved hole (raw type kept).
    Reserved(u32),
    /// `[0xFD_0000_0000, 1 TiB)` and a type-3 remap above it.
    HyperTransport,
    /// A hole type this generation does not define (reserved).
    Unknown(u32),
}

impl RegionKind {
    /// Only RAM may be handed to the OS as usable memory.
    pub const fn is_ram(self) -> bool {
        matches!(self, Self::Ram)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub base: u64,
    pub size: u64,
    pub kind: RegionKind,
}

impl Region {
    pub const fn end(&self) -> u64 {
        self.base + self.size
    }
}

/// Memory map errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapError {
    TooManyHoles,
    /// A hole wraps the address space.
    Wraps(Hole),
    /// Two holes overlap.
    Overlap(Hole, Hole),
    /// A type-3 hole above `0xFD_0000_0000`.
    Remap(Hole),
}

/// The physical memory map below `top` from the 9/9 holes, sorted, with
/// every non-RAM range listed (see the module documentation for the
/// differences to AGESA). Zero-sized holes are dropped.
pub fn memory_map(
    generation: Generation,
    top: u64,
    holes: impl IntoIterator<Item = Hole>,
) -> Result<Vec<Region, MAX_REGIONS>, MapError> {
    let mut holes: Vec<Hole, MAX_HOLES> = holes
        .into_iter()
        .filter(|h| h.size != 0)
        .try_fold(Vec::new(), |mut v, h| v.push(h).map(|_| v))
        .map_err(|_| MapError::TooManyHoles)?;
    if let Some(h) = holes.iter().find(|h| h.base.checked_add(h.size).is_none()) {
        return Err(MapError::Wraps(*h));
    }
    holes.sort_unstable_by_key(|h| h.base);
    if let Some(w) = holes.windows(2).find(|w| w[0].base + w[0].size > w[1].base) {
        return Err(MapError::Overlap(w[0], w[1]));
    }

    // Non-RAM intervals `(start, end, kind, synthetic)`; synthetic ones
    // give way to APOB holes at the same address.
    let mut spans: Vec<(u64, u64, RegionKind, bool), { 2 * MAX_HOLES + 1 }> = Vec::new();
    let mut push =
        |s: (u64, u64, RegionKind, bool)| spans.push(s).map_err(|_| MapError::TooManyHoles);
    for h in &holes {
        let end = h.base + h.size;
        match classify(generation, h.kind) {
            HoleClass::Uma => push((h.base, end, RegionKind::Uma, false))?,
            HoleClass::Mmio => push((h.base, end, RegionKind::Mmio, false))?,
            HoleClass::Reserved => push((h.base, end, RegionKind::Reserved(h.kind), false))?,
            HoleClass::Unknown => push((h.base, end, RegionKind::Unknown(h.kind), false))?,
            HoleClass::Remap1T if h.base > HT_HOLE_BASE => return Err(MapError::Remap(*h)),
            HoleClass::Remap1T => {
                push((h.base, HT_HOLE_BASE, RegionKind::Mmio, false))?;
                push((
                    HT_HOLE_BASE,
                    end.max(ONE_TIB),
                    RegionKind::HyperTransport,
                    false,
                ))?;
            }
        }
    }
    if top > HT_HOLE_BASE {
        push((HT_HOLE_BASE, ONE_TIB, RegionKind::HyperTransport, true))?;
    }
    spans.sort_unstable_by_key(|&(start, _, _, synthetic)| (start, synthetic));

    let mut out = Vec::new();
    let mut emit = |base: u64, end: u64, kind| {
        if end > base {
            // Capacity covers one RAM gap before each span plus the tail.
            let _ = out.push(Region {
                base,
                size: end - base,
                kind,
            });
        }
    };
    let cursor = spans.iter().fold(0, |cursor, &(start, end, kind, _)| {
        if end <= cursor {
            return cursor;
        }
        let start = start.max(cursor);
        emit(cursor, start.min(top), RegionKind::Ram);
        emit(start, end, kind);
        end
    });
    emit(cursor, top, RegionKind::Ram);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::build::{Body, Builder};
    use super::*;

    const GIB: u64 = 1 << 30;

    fn hole(base: u64, size: u64, kind: u32) -> Hole {
        Hole { base, size, kind }
    }

    fn ram(map: &[Region]) -> std::vec::Vec<(u64, u64)> {
        map.iter()
            .filter(|r| r.kind.is_ram())
            .map(|r| (r.base, r.end()))
            .collect()
    }

    #[test]
    fn parses_9_9() {
        let holes: std::vec::Vec<u8> = [
            (0xb000_0000u64, 0x5000_0000u64, 1u32),
            (0x2_0000_0000, 0x100_0000, 2),
        ]
        .iter()
        .flat_map(|&(b, s, t)| {
            [b.to_le_bytes(), s.to_le_bytes(), u64::from(t).to_le_bytes()].concat()
        })
        .collect();
        let body = Body::default()
            .at(0x30, &(64 * GIB).to_le_bytes())
            .at(0x38, &2u32.to_le_bytes())
            .at(0x40, &holes);
        let data = Builder::new(0x18).entry(9, 9, 0, &body.0).build();
        let apob = Apob::new(&data, Generation::Rome).unwrap();
        let map = system_memory_map(&apob).unwrap();
        assert_eq!(map.top, 64 * GIB);
        assert_eq!(
            map.holes().collect::<std::vec::Vec<_>>(),
            [
                hole(0xb000_0000, 0x5000_0000, 1),
                hole(0x2_0000_0000, 0x100_0000, 2)
            ]
        );

        let body = Body::default()
            .at(0x38, &19u32.to_le_bytes())
            .len(0x40 + 19 * 0x18);
        let data = Builder::new(0x18).entry(9, 9, 0, &body.0).build();
        let apob = Apob::new(&data, Generation::Milan).unwrap();
        assert!(matches!(
            system_memory_map(&apob),
            Err(ApobError::Value { value: 19, .. })
        ));
    }

    #[test]
    fn below_one_tib() {
        // MMIO hole below 4 GiB and a reserved hole, given out of order.
        let map = memory_map(
            Generation::Rome,
            0x10_5000_0000,
            [
                hole(0x8_0000_0000, 0x10_0000, 2),
                hole(0xb000_0000, 0x5000_0000, 1),
            ],
        )
        .unwrap();
        assert_eq!(
            map.as_slice(),
            [
                Region {
                    base: 0,
                    size: 0xb000_0000,
                    kind: RegionKind::Ram
                },
                Region {
                    base: 0xb000_0000,
                    size: 0x5000_0000,
                    kind: RegionKind::Mmio
                },
                Region {
                    base: 1 << 32,
                    size: 0x7_0000_0000,
                    kind: RegionKind::Ram
                },
                Region {
                    base: 0x8_0000_0000,
                    size: 0x10_0000,
                    kind: RegionKind::Reserved(2)
                },
                Region {
                    base: 0x8_0010_0000,
                    size: 0x10_5000_0000 - 0x8_0010_0000,
                    kind: RegionKind::Ram
                },
            ]
        );
    }

    #[test]
    fn one_tib_rules() {
        let top = ONE_TIB + 64 * GIB;
        let mmio = hole(0xb000_0000, 0x5000_0000, 1);
        let expected_ram = [(0, 0xb000_0000), (1 << 32, HT_HOLE_BASE), (ONE_TIB, top)];
        // Without any hole there (AGESA would not reserve it): still carved.
        let map = memory_map(Generation::Milan, top, [mmio]).unwrap();
        assert_eq!(ram(&map), expected_ram);
        assert!(map.contains(&Region {
            base: HT_HOLE_BASE,
            size: 12 * GIB,
            kind: RegionKind::HyperTransport
        }));
        // ABL reports it as a reserved hole: the hole wins.
        let map = memory_map(
            Generation::Rome,
            top,
            [mmio, hole(HT_HOLE_BASE, 12 * GIB, 2)],
        )
        .unwrap();
        assert_eq!(ram(&map), expected_ram);
        assert!(map.contains(&Region {
            base: HT_HOLE_BASE,
            size: 12 * GIB,
            kind: RegionKind::Reserved(2)
        }));
        // Type 3: MMIO up to 0xFD_0000_0000, then the 12 GiB.
        let map = memory_map(
            Generation::Milan,
            top,
            [mmio, hole(0xfc_0000_0000, 16 * GIB, 3)],
        )
        .unwrap();
        assert_eq!(
            ram(&map),
            [(0, 0xb000_0000), (1 << 32, 0xfc_0000_0000), (ONE_TIB, top)]
        );
        assert!(map.contains(&Region {
            base: 0xfc_0000_0000,
            size: 4 * GIB,
            kind: RegionKind::Mmio
        }));
        assert_eq!(
            memory_map(Generation::Rome, top, [hole(0xfe_0000_0000, GIB, 3)]),
            Err(MapError::Remap(hole(0xfe_0000_0000, GIB, 3)))
        );
    }

    #[test]
    fn per_generation_types() {
        let classes = |g| [2, 3, 4, 5, 6, 7, 8].map(|t| classify(g, t));
        use HoleClass::*;
        assert_eq!(
            classes(Generation::Naples),
            [
                Reserved, Unknown, Unknown, Unknown, Unknown, Unknown, Unknown
            ]
        );
        assert_eq!(
            classes(Generation::Rome),
            [
                Reserved, Remap1T, Reserved, Reserved, Unknown, Unknown, Unknown
            ]
        );
        assert_eq!(
            classes(Generation::Milan),
            [
                Reserved, Remap1T, Unknown, Unknown, Reserved, Reserved, Unknown
            ]
        );
        // An unknown type is never RAM.
        let map = memory_map(Generation::Milan, 8 * GIB, [hole(4 * GIB, GIB, 4)]).unwrap();
        assert_eq!(ram(&map), [(0, 4 * GIB), (5 * GIB, 8 * GIB)]);
        assert!(map.contains(&Region {
            base: 4 * GIB,
            size: GIB,
            kind: RegionKind::Unknown(4)
        }));
    }

    #[test]
    fn rejects_bad_holes() {
        let a = hole(0x1000, 0x2000, 2);
        let b = hole(0x2000, 0x1000, 1);
        assert_eq!(
            memory_map(Generation::Rome, GIB, [a, b]),
            Err(MapError::Overlap(a, b))
        );
        let many = (0..19).map(|i| hole(i * 0x10000, 0x1000, 2));
        assert_eq!(
            memory_map(Generation::Rome, GIB, many),
            Err(MapError::TooManyHoles)
        );
        let wrap = hole(u64::MAX, 2, 1);
        assert_eq!(
            memory_map(Generation::Rome, GIB, [wrap]),
            Err(MapError::Wraps(wrap))
        );
    }
}
