// SPDX-License-Identifier: MIT OR Apache-2.0
//! Rs (Siena/Genoa) APOB schemas, independently bound to the recovered
//! ApobRsPei and memory consumers. Not a Milan mailbox/record alias.
//! Container version >=5; topology: 12 CCD x 2 complexes x 8 cores x SMT2.
//! Memory9/9: 49 packed24-byte holes. Capacities are not live population.
use super::sysmap::{HoleClass, MemoryFormat};
use super::{Apob, ApobError, Format, ccx::CoreMap, group, instance};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rs;
impl Format for Rs {
    fn first_entry(self, version: u32, first: u32) -> Result<u32, ApobError> {
        if version < 5 {
            Err(ApobError::Version(version))
        } else {
            Ok(first)
        }
    }
}
/// The PSP's ACPI error-injection buffer (openSIL `ReservedRasEinj`).
pub const HOLE_RAS_EINJ: u32 = 15;

impl MemoryFormat for Rs {
    const MAX_HOLES: usize = 49;
    fn classify(self, kind: u32) -> HoleClass {
        match kind {
            0 => HoleClass::Uma,
            1 => HoleClass::Mmio,
            3 => HoleClass::Remap1T,
            2 | 4..=15 => HoleClass::Reserved,
            _ => HoleClass::Unknown,
        }
    }
}

pub mod memory;

/// The actual NUMA nodes per socket, APOB 9/13h (+31h ActualNps, after the
/// requested value; openSIL APOB_SYSTEM_NPS_INFO_TYPE_STRUCT, the same
/// offsets and encoding as SP3's).
pub fn nps(apob: &Apob<'_, Rs>) -> Result<super::nps::Nps, ApobError> {
    use super::nps::Nps;
    match apob.find(group::FABRIC, 0x13, 0)?.byte(0x31)? {
        0 => Ok(Nps::Nps0),
        1 => Ok(Nps::Nps1),
        2 => Ok(Nps::Nps2),
        3 => Ok(Nps::Nps4),
        value => Err(ApobError::Value {
            group: group::FABRIC,
            kind: 0x13,
            value: value.into(),
        }),
    }
}

pub const CCDS: usize = 12;
/// Preserve logical record indices as well as physical IDs: AP release
/// uses logical complex/core/thread bits but a physical CCD SMN target.
pub fn core_map(apob: &Apob<'_, Rs>, socket: u8) -> Result<CoreMap<CCDS>, ApobError> {
    let map = CoreMap::<CCDS>::parse_entry::<8>(apob.find(group::CCX, 3, instance(socket, 0))?)?;
    let bad = |value: u8| ApobError::Value {
        group: group::CCX,
        kind: 3,
        value: value.into(),
    };
    let mut ccds = 0u16;
    for d in &map.ccds {
        if d.id >= 12 || ccds & (1 << d.id) != 0 {
            return Err(bad(d.id));
        }
        ccds |= 1 << d.id;
        let mut complexes = 0u8;
        for x in &d.ccxs {
            if x.id >= 2 || complexes & (1 << x.id) != 0 {
                return Err(bad(x.id));
            }
            complexes |= 1 << x.id;
            let mut cores = 0u8;
            for c in &x.cores {
                if c.id >= 8 || cores & (1 << c.id) != 0 {
                    return Err(bad(c.id));
                }
                cores |= 1 << c.id;
            }
        }
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apob::{
        build::{Body, Builder},
        sysmap,
    };

    fn topology() -> std::vec::Vec<u8> {
        let mut out = vec![0xff; CCDS * 0x33];
        // Deliberately sparse logical slots and non-identical physical IDs.
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
    fn twelve_slot_map_keeps_both_logical_and_physical_identities() {
        let bytes = Builder::new(0x21).entry(3, 3, 0, &topology()).build();
        let apob = Apob::new(&bytes, Rs).unwrap();
        let map = core_map(&apob, 0).unwrap();
        assert_eq!(map.threads(), 128);
        assert_eq!(
            map.ccds
                .iter()
                .map(|d| (d.logical, d.id))
                .collect::<std::vec::Vec<_>>(),
            [(0, 0), (1, 3), (8, 8), (11, 11)]
        );
        assert_eq!(
            (
                map.ccds[1].ccxs[1].logical,
                map.ccds[1].ccxs[1].cores[7].logical
            ),
            (1, 7)
        );
        assert!(core_map(&apob, 1).is_err());
        let short = Builder::new(0x21)
            .entry(3, 3, 0, &topology()[..8 * 0x33])
            .build();
        assert!(core_map(&Apob::new(&short, Rs).unwrap(), 0).is_err());
    }

    #[test]
    fn rejects_unsupported_version_duplicate_ids_and_malformed_records() {
        let old = Builder::new(4).entry(3, 3, 0, &topology()).build();
        assert!(matches!(Apob::new(&old, Rs), Err(ApobError::Version(4))));
        for (offset, value) in [(0x33, 0), (0, 12), (1, 2), (2, 8)] {
            let mut body = topology();
            body[offset] = value;
            let bytes = Builder::new(5).entry(3, 3, 0, &body).build();
            assert!(core_map(&Apob::new(&bytes, Rs).unwrap(), 0).is_err());
        }
        let mut bytes = Builder::new(5).entry(3, 3, 0, &topology()).build();
        bytes[0x1c..0x20].fill(0);
        assert!(matches!(
            Apob::new(&bytes, Rs),
            Err(ApobError::Entry { .. })
        ));
    }

    #[test]
    fn rs_capacity_and_reserved_types_are_not_the_sp3_policy() {
        let holes: std::vec::Vec<u8> = (0..49u64)
            .flat_map(|i| {
                [
                    (0x1_0000_0000 + i * 0x2000).to_le_bytes(),
                    0x1000u64.to_le_bytes(),
                    12u64.to_le_bytes(),
                ]
                .concat()
            })
            .collect();
        let body = Body::default()
            .at(0x30, &(64u64 << 30).to_le_bytes())
            .at(0x38, &49u32.to_le_bytes())
            .at(0x40, &holes);
        let bytes = Builder::new(5).entry(9, 9, 0, &body.0).build();
        let apob = Apob::new(&bytes, Rs).unwrap();
        let memory = sysmap::system_memory_map(&apob).unwrap();
        assert_eq!(memory.holes().count(), 49);
        let regions = sysmap::memory_map(Rs, memory.top, memory.holes()).unwrap();
        assert_eq!(
            regions
                .iter()
                .filter(|r| r.kind == sysmap::RegionKind::Reserved(12))
                .count(),
            49
        );
        let legacy = Apob::new(&bytes, crate::psp::Variant::Gn).unwrap();
        assert!(sysmap::system_memory_map(&legacy).is_err());
        for kind in [2, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15] {
            assert_eq!(Rs.classify(kind), HoleClass::Reserved);
        }
        assert_eq!(Rs.classify(16), HoleClass::Unknown);
        assert_eq!(sysmap::memory_map(Rs, 0, []), Err(sysmap::MapError::NoDram));
        let body = Body::default()
            .at(0x38, &50u32.to_le_bytes())
            .len(0x40 + 50 * 0x18);
        let bytes = Builder::new(5).entry(9, 9, 0, &body.0).build();
        assert!(sysmap::system_memory_map(&Apob::new(&bytes, Rs).unwrap()).is_err());
    }
}
