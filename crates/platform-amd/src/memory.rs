// SPDX-License-Identifier: MIT OR Apache-2.0
//! The memory map of every SoC: e820 from the APOB's memory map, the
//! TOM/TOM2 limits and the firmware's reservations (PSP_APOB §3.5, §3.6).
//!
//! [`fstart_driver_amd::apob::sysmap::memory_map`] turns the APOB's 9/9
//! holes into RAM and non-RAM regions (with the 1 TiB rule). [`e820`]
//! checks those against the TOM/TOM2 MSRs, overlays what the firmware
//! keeps from the OS (the SoC's [`Reservation`]s: the legacy area and low
//! scratch, the APOB, the firmware's DRAM, TSEG) and returns a sorted,
//! merged e820 list. It is a pure function so it can be tested on the
//! host; the flow reads the MSRs and the APOB.
//!
//! Differences to AGESA/AMI (AMI MemDetect, PSP_APOB §3.6):
//! - MMIO holes are left out of e820 (AMI reports them in its HOBs; the OS
//!   needs the gaps for PCI windows), every other hole type is reserved;
//! - the firmware's whole DRAM ([`crate::layout::Layout::early`]) stays
//!   reserved (AMI only reserves the reset image): page tables, parked APs
//!   and the AP route of the reset image live there.

use crate::layout::Region;
use fstart_core::services::memory_detect::{E820Entry, E820Kind, E820State};
use fstart_driver_amd::apob::sysmap::{self, RegionKind};
use heapless::Vec;

/// 4 GiB.
pub const FOUR_GIB: u64 = 1 << 32;
/// Most reservations [`e820`] takes.
pub const MAX_RESERVATIONS: usize = 16;
const MAX_POINTS: usize = 2 * (fstart_driver_amd::apob::sysmap::MAX_REGIONS + MAX_RESERVATIONS);

/// A range firmware keeps from the OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reservation {
    pub span: Region,
    pub kind: E820Kind,
}

/// The top-of-memory MSRs: `TOP_MEM` (C001_001A) and, when SYSCFG.Tom2En is
/// set, `TOM2` (C001_001D). The PSP/ABL program both before x86 runs
/// (PSP_APOB §3.6); DRAM is only decoded below TOM and in `[4 GiB, TOM2)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TopOfMemory {
    pub tom: u64,
    pub tom2: Option<u64>,
}

impl TopOfMemory {
    fn decodes(&self, span: Region) -> bool {
        span.end() <= self.tom
            || self
                .tom2
                .is_some_and(|tom2| span.base >= FOUR_GIB && span.end() <= tom2)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryError {
    /// APOB RAM outside what TOM/TOM2 decode as DRAM.
    NotDecoded(Region),
    /// TOM above 4 GiB, or TOM2 at or below 4 GiB.
    Limits(TopOfMemory),
    /// A reservation over an MMIO hole or outside the APOB map.
    ReservationOutsideDram(Region),
    TooManyEntries,
}

impl Reservation {
    /// `span` reserved.
    pub const fn reserved(span: Region) -> Self {
        Self {
            span,
            kind: E820Kind::Reserved,
        }
    }
}

/// The highest `size`-aligned window of `size` bytes inside one RAM region
/// below `tom` (and 4 GiB) that overlaps none of `avoid`: TSEG.
pub fn place_tseg(
    regions: &[sysmap::Region],
    tom: u64,
    size: u64,
    avoid: &[Region],
) -> Option<Region> {
    let limit = tom.min(FOUR_GIB);
    let fit = |r: &sysmap::Region| {
        let mut top = r.end().min(limit) & !(size - 1);
        while top >= r.base + size {
            let candidate = Region::new(top - size, size);
            match avoid.iter().find(|a| a.overlaps(&candidate)) {
                Some(a) => top = a.base & !(size - 1),
                None => return Some(candidate),
            }
        }
        None
    };
    regions
        .iter()
        .filter(|r| r.kind.is_ram() && r.base < limit)
        .filter_map(fit)
        .max_by_key(|s| s.base)
}

/// The e820 map of `regions` with `reservations` ([`e820`]), logged.
pub fn build(
    name: &str,
    regions: &[sysmap::Region],
    limits: TopOfMemory,
    reservations: &[Reservation],
) -> Result<E820State, MemoryError> {
    let mut map = E820State::new();
    let count = e820(regions, limits, reservations, map.entries_mut())?;
    let ram: u64 = map.entries_mut()[..count]
        .iter()
        .filter(|e| e.kind == E820Kind::Ram as u32)
        .map(|e| e.size)
        .sum();
    map.set_detected(count, ram)
        .map_err(|_| MemoryError::TooManyEntries)?;
    for e in map.entries() {
        let (addr, size, kind) = (e.addr, e.size, e.kind);
        fstart_log::info!("e820: {:#x}+{:#x} type {}", addr, size, kind);
    }
    fstart_log::info!("{}: {} MiB RAM", name, ram >> 20);
    Ok(map)
}

/// What covers an address: higher wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Use {
    Ram,
    Nvs,
    Reserved,
}

impl Use {
    fn kind(self) -> E820Kind {
        match self {
            Self::Ram => E820Kind::Ram,
            Self::Nvs => E820Kind::Nvs,
            Self::Reserved => E820Kind::Reserved,
        }
    }

    fn of_reservation(kind: E820Kind) -> Self {
        match kind {
            E820Kind::Ram => Self::Ram,
            E820Kind::Nvs => Self::Nvs,
            _ => Self::Reserved,
        }
    }
}

fn span(region: &sysmap::Region) -> Region {
    Region::new(region.base, region.size)
}

/// Build the e820 map into `out`; returns the number of entries.
///
/// `regions` is the output of `apob::sysmap::memory_map` for the running
/// silicon. Every RAM region must be decoded by TOM/TOM2, and every
/// reservation must lie in DRAM (RAM or a reserved DRAM hole), never in an
/// MMIO hole or above the APOB top.
pub fn e820(
    regions: &[sysmap::Region],
    limits: TopOfMemory,
    reservations: &[Reservation],
    out: &mut [E820Entry],
) -> Result<usize, MemoryError> {
    if limits.tom > FOUR_GIB || limits.tom2.is_some_and(|tom2| tom2 <= FOUR_GIB) {
        return Err(MemoryError::Limits(limits));
    }
    if let Some(r) = regions
        .iter()
        .find(|r| r.kind.is_ram() && !limits.decodes(span(r)))
    {
        return Err(MemoryError::NotDecoded(span(r)));
    }
    let is_mmio = |r: &sysmap::Region| matches!(r.kind, RegionKind::Mmio);
    let top = regions.iter().map(sysmap::Region::end).max().unwrap_or(0);
    if let Some(r) = reservations.iter().find(|res| {
        res.span.end() > top
            || regions
                .iter()
                .any(|r| is_mmio(r) && span(r).overlaps(&res.span))
    }) {
        return Err(MemoryError::ReservationOutsideDram(r.span));
    }

    // Base layer: RAM, and every non-MMIO hole reserved.
    let base = regions.iter().filter(|r| !is_mmio(r)).map(|r| {
        let usage = if r.kind.is_ram() {
            Use::Ram
        } else {
            Use::Reserved
        };
        (span(r), usage)
    });
    let overlay = reservations
        .iter()
        .map(|r| (r.span, Use::of_reservation(r.kind)));
    let layers: Vec<(Region, Use), MAX_POINTS> = base
        .chain(overlay)
        .filter(|(s, _)| s.size != 0)
        .try_fold(Vec::new(), |mut v, l| v.push(l).map(|_| v))
        .map_err(|_| MemoryError::TooManyEntries)?;

    let mut points: Vec<u64, MAX_POINTS> = Vec::new();
    for (s, _) in &layers {
        points
            .extend_from_slice(&[s.base, s.end()])
            .map_err(|_| MemoryError::TooManyEntries)?;
    }
    points.sort_unstable();

    // Each elementary interval takes the strongest covering use; equal
    // neighbours merge.
    let count = points
        .windows(2)
        .filter(|w| w[0] < w[1])
        .filter_map(|w| {
            let piece = Region::new(w[0], w[1] - w[0]);
            layers
                .iter()
                .filter(|(s, _)| s.contains(&piece))
                .map(|&(_, u)| u)
                .max()
                .map(|u| (piece, u))
        })
        .try_fold(0usize, |n, (piece, usage)| {
            let kind = usage.kind() as u32;
            if let Some(last) = n.checked_sub(1).map(|i| &mut out[i])
                && last.kind == kind
                && last.addr + last.size == piece.base
            {
                last.size += piece.size;
                return Ok(n);
            }
            let slot = out.get_mut(n).ok_or(MemoryError::TooManyEntries)?;
            *slot = E820Entry::new(piece.base, piece.size, usage.kind());
            Ok(n + 1)
        })?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sp3::silicon::Generation;
    use fstart_driver_amd::apob::sysmap::{Hole, memory_map};
    const TSEG_SIZE: u64 = 8 << 20;

    const GIB: u64 = 1 << 30;

    fn entries(out: &[E820Entry]) -> std::vec::Vec<(u64, u64, u32)> {
        out.iter().map(|e| (e.addr, e.size, e.kind)).collect()
    }

    /// A 1P Rome-like map: 2 GiB MMIO below 4 GiB, DRAM to 1.5 TiB with the
    /// HyperTransport hole, a reserved type-2 hole above 4 GiB.
    fn rome_map() -> std::vec::Vec<sysmap::Region> {
        let holes = [
            Hole {
                base: 0x8000_0000,
                size: 0x8000_0000,
                kind: 1,
            },
            Hole {
                base: 0x1_0000_0000 + 4 * GIB,
                size: 0x1000_0000,
                kind: 2,
            },
        ];
        memory_map(Generation::Rome.silicon().into(), 1536 * GIB, holes)
            .unwrap()
            .to_vec()
    }

    #[test]
    fn rome_map_with_reservations() {
        let regions = rome_map();
        let limits = TopOfMemory {
            tom: 0x8000_0000,
            tom2: Some(1536 * GIB),
        };
        let tseg = Region::new(0x7c00_0000, 0x400_0000);
        let res = crate::sp3::memory::reservations(
            Generation::Rome,
            Region::new(0x3000_0000, 0x1_2345),
            Some(tseg),
        );
        let mut out = [E820Entry::zeroed(); 32];
        let n = e820(&regions, limits, &res, &mut out).unwrap();
        let ram = E820Kind::Ram as u32;
        let rsv = E820Kind::Reserved as u32;
        let nvs = E820Kind::Nvs as u32;
        assert_eq!(
            entries(&out[..n]),
            [
                (0, 0xf000, rsv),
                (0xf000, 0xa_0000 - 0xf000, ram),
                (0xa_0000, 0x6_0000, rsv),
                (0x10_0000, 0x3000_0000 - 0x10_0000, ram),
                (0x3000_0000, 0x1_3000, nvs),
                (0x3001_3000, 0x7500_0000 - 0x3001_3000, ram),
                (0x7500_0000, 0x100_0000, rsv),
                (0x7600_0000, 0x7c00_0000 - 0x7600_0000, ram),
                (0x7c00_0000, 0x400_0000, rsv),
                // MMIO hole [2 GiB, 4 GiB) is a gap.
                (FOUR_GIB, 4 * GIB, ram),
                (FOUR_GIB + 4 * GIB, 0x1000_0000, rsv),
                (
                    FOUR_GIB + 4 * GIB + 0x1000_0000,
                    0xfd_0000_0000 - (8 * GIB + 0x1000_0000),
                    ram
                ),
                (0xfd_0000_0000, 12 * GIB, rsv),
                (1 << 40, 512 * GIB, ram),
            ]
        );
    }

    #[test]
    fn limits_and_reservations_fail_closed() {
        let regions = rome_map();
        let mut out = [E820Entry::zeroed(); 32];
        // TOM below the APOB's first hole: RAM [TOM, 2 GiB) is not decoded.
        let low_tom = TopOfMemory {
            tom: 0x7000_0000,
            tom2: Some(1536 * GIB),
        };
        assert!(matches!(
            e820(&regions, low_tom, &[], &mut out),
            Err(MemoryError::NotDecoded(_))
        ));
        let no_tom2 = TopOfMemory {
            tom: 0x8000_0000,
            tom2: None,
        };
        assert!(matches!(
            e820(&regions, no_tom2, &[], &mut out),
            Err(MemoryError::NotDecoded(_))
        ));
        let bad = TopOfMemory {
            tom: 0x8000_0000,
            tom2: Some(FOUR_GIB),
        };
        assert!(matches!(
            e820(&regions, bad, &[], &mut out),
            Err(MemoryError::Limits(_))
        ));
        let ok = TopOfMemory {
            tom: 0x8000_0000,
            tom2: Some(1536 * GIB),
        };
        let in_mmio = [Reservation {
            span: Region::new(0xfe00_0000, 0x1000),
            kind: E820Kind::Reserved,
        }];
        assert!(matches!(
            e820(&regions, ok, &in_mmio, &mut out),
            Err(MemoryError::ReservationOutsideDram(_))
        ));
        let mut tiny = [E820Entry::zeroed(); 4];
        assert_eq!(
            e820(&regions, ok, &[], &mut tiny),
            Err(MemoryError::TooManyEntries)
        );
    }

    fn ram(base: u64, size: u64) -> sysmap::Region {
        sysmap::Region {
            base,
            size,
            kind: RegionKind::Ram,
        }
    }

    #[test]
    fn tseg_goes_to_the_top_of_low_ram_around_firmware() {
        let regions = [
            ram(0, 0x8000_0000),
            sysmap::Region {
                base: 0x8000_0000,
                size: 0x8000_0000,
                kind: RegionKind::Mmio,
            },
            ram(FOUR_GIB, 0x1_0000_0000),
        ];
        let early = crate::sp3::layout::LAYOUT.early;
        let t = place_tseg(&regions, 0x8000_0000, TSEG_SIZE, &[early]).unwrap();
        assert_eq!(t, Region::new(0x7f80_0000, TSEG_SIZE));
        // TOM inside the firmware window: step below it.
        let t = place_tseg(&regions, 0x7600_0000, TSEG_SIZE, &[early]).unwrap();
        assert_eq!(t, Region::new(0x7480_0000, TSEG_SIZE));
        assert_eq!(place_tseg(&regions[1..], 0x8000_0000, 8 << 20, &[]), None);
    }
}
