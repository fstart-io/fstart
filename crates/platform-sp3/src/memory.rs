//! e820 from the APOB memory map, the TOM/TOM2 limits and the firmware's
//! reservations (PSP_APOB §3.5, §3.6).
//!
//! [`fstart_driver_amd::sp3::apob::sysmap::memory_map`] already turns the 9/9
//! holes into RAM and non-RAM regions (with the 1 TiB rule). This module
//! checks those against the TOM/TOM2 MSRs, overlays what firmware must keep
//! (legacy area, x86 low scratch, APOB, the reset image and early region,
//! TSEG) and returns a sorted, merged e820 list. It is a pure function so it
//! can be tested on the host; the flow reads the MSRs and the APOB.
//!
//! Differences to AGESA/AMI (AMI MemDetect, PSP_APOB §3.6):
//! - MMIO holes are left out of e820 (AMI reports them in its HOBs; the OS
//!   needs the gaps for PCI windows), every other hole type is reserved;
//! - the APOB is ACPI NVS on Rome/Milan as AGESA reports it, and reserved on
//!   Naples (AGESA: boot-services data, i.e. RAM for the OS) because the
//!   firmware may still read it after the payload starts;
//! - the whole 16 MiB window below `0x7600_0000` stays reserved (AMI only
//!   reserves the reset image): page tables, parked APs and the AP route of
//!   the reset image live there.

use fstart_core::services::memory_detect::{E820Entry, E820Kind};
use fstart_driver_amd::sp3::apob::sysmap::{Region, RegionKind};
use fstart_driver_amd::sp3::soc::Generation;
use heapless::Vec;

use crate::layout::{CACHE_WINDOW, Span};

/// 4 GiB.
pub const FOUR_GIB: u64 = 1 << 32;
/// Legacy VGA/option ROM/BIOS area, reserved like AMI's MemDetect.
pub const LEGACY_HOLE: Span = Span::new(0xa_0000, 0x6_0000);
/// Page zero and the x86 low scratch (the arch crate's SIPI trampoline page
/// `0x8000` and S3/postcar stash): `[0, LOW_SCRATCH_END)`.
pub const LOW_RESERVED: Span = Span::new(0, 0xf000);

/// Most reservations [`e820`] takes.
pub const MAX_RESERVATIONS: usize = 16;
const MAX_POINTS: usize = 2 * (fstart_driver_amd::sp3::apob::sysmap::MAX_REGIONS + MAX_RESERVATIONS);

/// A range firmware keeps from the OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reservation {
    pub span: Span,
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
    fn decodes(&self, span: Span) -> bool {
        span.end() <= self.tom
            || self
                .tom2
                .is_some_and(|tom2| span.base >= FOUR_GIB && span.end() <= tom2)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryError {
    /// APOB RAM outside what TOM/TOM2 decode as DRAM.
    NotDecoded(Span),
    /// TOM above 4 GiB, or TOM2 at or below 4 GiB.
    Limits(TopOfMemory),
    /// A reservation over an MMIO hole or outside the APOB map.
    ReservationOutsideDram(Span),
    TooManyEntries,
}

/// The reservations every SP3 boot keeps: page zero and low scratch, the
/// legacy hole, the APOB `[base, roundup(size, 4 KiB))`, the early region
/// with the reset image, and TSEG when SMM is set up.
pub fn standard_reservations(
    generation: Generation,
    apob: Span,
    tseg: Option<Span>,
) -> Vec<Reservation, MAX_RESERVATIONS> {
    let apob_kind = match generation {
        Generation::Naples => E820Kind::Reserved,
        Generation::Rome | Generation::Milan => E820Kind::Nvs,
    };
    let apob = Span::new(apob.base, apob.size.next_multiple_of(0x1000));
    let reserved = |span| Reservation {
        span,
        kind: E820Kind::Reserved,
    };
    [
        reserved(LOW_RESERVED),
        reserved(LEGACY_HOLE),
        Reservation {
            span: apob,
            kind: apob_kind,
        },
        reserved(CACHE_WINDOW),
    ]
    .into_iter()
    .chain(tseg.map(reserved))
    .filter(|r| r.span.size != 0)
    .collect()
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

fn span(region: &Region) -> Span {
    Span::new(region.base, region.size)
}

/// Build the e820 map into `out`; returns the number of entries.
///
/// `regions` is the output of `apob::sysmap::memory_map` for the running
/// generation. Every RAM region must be decoded by TOM/TOM2, and every
/// reservation must lie in DRAM (RAM or a reserved DRAM hole), never in an
/// MMIO hole or above the APOB top.
pub fn e820(
    regions: &[Region],
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
    let is_mmio = |r: &Region| matches!(r.kind, RegionKind::Mmio);
    let top = regions.iter().map(Region::end).max().unwrap_or(0);
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
    let layers: Vec<(Span, Use), MAX_POINTS> = base
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
            let piece = Span::new(w[0], w[1] - w[0]);
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
    use fstart_driver_amd::sp3::apob::sysmap::{Hole, memory_map};

    const GIB: u64 = 1 << 30;

    fn entries(out: &[E820Entry]) -> std::vec::Vec<(u64, u64, u32)> {
        out.iter().map(|e| (e.addr, e.size, e.kind)).collect()
    }

    /// A 1P Rome-like map: 2 GiB MMIO below 4 GiB, DRAM to 1.5 TiB with the
    /// HyperTransport hole, a reserved type-2 hole above 4 GiB.
    fn rome_map() -> std::vec::Vec<Region> {
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
        memory_map(Generation::Rome, 1536 * GIB, holes)
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
        let tseg = Span::new(0x7c00_0000, 0x400_0000);
        let res = standard_reservations(
            Generation::Rome,
            Span::new(0x3000_0000, 0x1_2345),
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
            span: Span::new(0xfe00_0000, 0x1000),
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

    #[test]
    fn naples_apob_is_reserved_not_nvs() {
        let res = standard_reservations(Generation::Naples, Span::new(0x3000_0000, 0x1000), None);
        assert_eq!(res.len(), 4);
        assert_eq!(res[2].kind, E820Kind::Reserved);
        assert_eq!(res[3].span, CACHE_WINDOW);
    }
}
