// SPDX-License-Identifier: MIT OR Apache-2.0
//! The RS (Genoa, Siena) APOB records that differ from Milan's. The
//! container, the 9/9 memory map and the 9/13h NPS are Milan's: an RS APOB
//! is read as [`super::Variant::Gn`]'s (version 5 and later; a hole type
//! Milan does not define, such as RS's 11, 14 and 15, stays reserved).
//!
//! - [`core_map`]: 3/3 with twelve CCD slots of two complexes of eight
//!   cores. The AP release addresses a thread by its logical slots (the
//!   record positions) on its physical CCD, so both are kept.
//! - [`memory`]: Genoa's 1/17h DDR5 SPD copies and 8/8 DIMM positions.
//!
//! Recovered from the S8040's ApobRsPei and its memory consumers; the
//! S8040 and the Onyx boot with them.
use heapless::Vec;
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

use super::{Apob, ApobError, group, instance};

pub mod memory;

/// The 9/9 hole of the PSP's ACPI EINJ command buffer (openSIL
/// `ReservedRasEinj`, `xUSL/Include/ApobCmn.h:429`; [`crate::psp::einj`]).
pub const HOLE_RAS_EINJ: u32 = 15;

/// CCD slots of the 3/3 record.
pub const CCDS: usize = 12;
const CCXS: usize = 2;
const CORES: usize = 8;
const ABSENT: u8 = super::ccx::ABSENT;

#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct RawCore {
    id: u8,
    threads: [u8; 2],
}

#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct RawCcx {
    id: u8,
    cores: [RawCore; CORES],
}

#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct RawCcd {
    id: u8,
    ccxs: [RawCcx; CCXS],
}

/// A present core: its slot in the complex and its physical ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Core {
    pub logical: u8,
    pub id: u8,
    pub threads: [bool; 2],
}

/// A present L3 complex.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ccx {
    pub logical: u8,
    pub id: u8,
    pub cores: Vec<Core, CORES>,
}

/// A present CCD: its slot and its physical ID (the `0x2000000` SMN stride
/// of its registers).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ccd {
    pub logical: u8,
    pub id: u8,
    pub ccxs: Vec<Ccx, CCXS>,
}

/// The present CCDs, complexes and cores of one socket, in record order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CoreMap {
    pub ccds: Vec<Ccd, CCDS>,
}

impl CoreMap {
    /// Enabled threads.
    pub fn threads(&self) -> usize {
        self.cores()
            .map(|(_, _, c)| c.threads.iter().filter(|&&t| t).count())
            .sum()
    }

    /// Every present core with its CCD and complex.
    pub fn cores(&self) -> impl Iterator<Item = (&Ccd, &Ccx, &Core)> {
        self.ccds.iter().flat_map(|d| {
            d.ccxs
                .iter()
                .flat_map(move |x| x.cores.iter().map(move |c| (d, x, c)))
        })
    }
}

/// The 3/3 record of `socket`. A physical ID out of range or seen twice is
/// an error.
pub fn core_map(apob: &Apob<'_>, socket: u8) -> Result<CoreMap, ApobError> {
    let entry = apob.find(group::CCX, 3, instance(socket, 0))?;
    let raw: &[RawCcd; CCDS] = entry.at(0x30)?;
    let bad = |value: u8| ApobError::Value {
        group: group::CCX,
        kind: 3,
        value: value.into(),
    };
    let unique = |ids: &mut u16, id: u8, limit: usize| {
        if usize::from(id) >= limit || *ids & 1 << id != 0 {
            return Err(bad(id));
        }
        *ids |= 1 << id;
        Ok(())
    };
    let mut map = CoreMap::default();
    let mut ccd_ids = 0;
    for (logical, d) in (0u8..).zip(raw).filter(|(_, d)| d.id != ABSENT) {
        unique(&mut ccd_ids, d.id, CCDS)?;
        let mut ccd = Ccd {
            logical,
            id: d.id,
            ccxs: Vec::new(),
        };
        let mut ccx_ids = 0;
        for (logical, x) in (0u8..).zip(&d.ccxs).filter(|(_, x)| x.id != ABSENT) {
            unique(&mut ccx_ids, x.id, CCXS)?;
            let mut ccx = Ccx {
                logical,
                id: x.id,
                cores: Vec::new(),
            };
            let mut core_ids = 0;
            for (logical, c) in (0u8..).zip(&x.cores).filter(|(_, c)| c.id != ABSENT) {
                unique(&mut core_ids, c.id, CORES)?;
                // The capacities are the record's array lengths.
                let _ = ccx.cores.push(Core {
                    logical,
                    id: c.id,
                    threads: c.threads.map(|t| t != 0),
                });
            }
            let _ = ccd.ccxs.push(ccx);
        }
        let _ = map.ccds.push(ccd);
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apob::build::Builder;
    use crate::psp::Variant;

    fn topology() -> std::vec::Vec<u8> {
        let mut out = vec![0xff; CCDS * 0x33];
        // Sparse logical slots, physical IDs that differ from them.
        for (slot, physical) in [(0, 0), (1, 3), (8, 8), (11, 11)] {
            let start = slot * 0x33;
            out[start] = physical;
            for x in 0..2 {
                let complex = start + 1 + x * 0x19;
                out[complex] = x as u8;
                for core in 0..8 {
                    let offset = complex + 1 + core * 3;
                    out[offset..offset + 3].copy_from_slice(&[core as u8, 1, 1]);
                }
            }
        }
        out
    }

    #[test]
    fn twelve_slots_keep_logical_and_physical_ids() {
        let bytes = Builder::new(0x21).entry(3, 3, 0, &topology()).build();
        let apob = Apob::new(&bytes, Variant::Gn).unwrap();
        let map = core_map(&apob, 0).unwrap();
        assert_eq!(map.threads(), 128);
        assert_eq!(
            map.ccds
                .iter()
                .map(|d| (d.logical, d.id))
                .collect::<std::vec::Vec<_>>(),
            [(0, 0), (1, 3), (8, 8), (11, 11)]
        );
        assert_eq!(map.ccds[1].ccxs[1].cores[7].logical, 7);
        assert!(core_map(&apob, 1).is_err());
        let short = Builder::new(0x21)
            .entry(3, 3, 0, &topology()[..8 * 0x33])
            .build();
        assert!(core_map(&Apob::new(&short, Variant::Gn).unwrap(), 0).is_err());
    }

    #[test]
    fn rejects_duplicate_and_out_of_range_ids() {
        for (offset, value) in [(0x33, 0), (0, 12), (1, 2), (2, 8)] {
            let mut body = topology();
            body[offset] = value;
            let bytes = Builder::new(5).entry(3, 3, 0, &body).build();
            assert!(core_map(&Apob::new(&bytes, Variant::Gn).unwrap(), 0).is_err());
        }
    }

    #[test]
    fn the_milan_rules_read_the_rs_memory_map() {
        use crate::apob::build::Body;
        use crate::apob::sysmap::{self, RegionKind};
        // The S8040's 9/9: MMIO below 4 GiB and RS hole types 11, 15, 14, 2.
        let holes: std::vec::Vec<u8> = [
            (0xb000_0000u64, 0x5000_0000u64, 1u64),
            (0x60_4d8c_0000, 0x104_0000, 11),
            (0x60_4e90_0000, 0x10_0000, 15),
            (0x60_4ea0_0000, 0x20_0000, 14),
            (0x60_4ec0_0000, 0x140_0000, 2),
        ]
        .iter()
        .flat_map(|&(b, s, t)| [b.to_le_bytes(), s.to_le_bytes(), t.to_le_bytes()].concat())
        .collect();
        let body = Body::default()
            .at(0x30, &0x60_5000_0000u64.to_le_bytes())
            .at(0x38, &5u32.to_le_bytes())
            .at(0x40, &holes)
            .len(0x40 + 49 * 0x18);
        let bytes = Builder::new(0x18).entry(9, 9, 0, &body.0).build();
        let apob = Apob::new(&bytes, Variant::Gn).unwrap();
        let memory = sysmap::system_memory_map(&apob).unwrap();
        let regions = sysmap::memory_map(Variant::Gn, memory.top, memory.holes()).unwrap();
        let ram: u64 = regions
            .iter()
            .filter(|r| r.kind.is_ram())
            .map(|r| r.size)
            .sum();
        assert_eq!(ram, 0x60_4d8c_0000 - 0x5000_0000);
        assert!(regions.iter().all(|r| r.kind.is_ram()
            || matches!(
                r.kind,
                RegionKind::Mmio | RegionKind::Reserved(2) | RegionKind::Unknown(11 | 14 | 15)
            )));
    }
}
