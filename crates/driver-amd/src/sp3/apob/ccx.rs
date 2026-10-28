//! APOB 3/3: the physical CCD/CCX/core map of one socket (PSP_APOB §3.5,
//! CCX_SP3 §3.7).
//!
//! Instance `socket << 8`, right after the entry header: 8 CCDs ×
//! `{id; 2 CCX × {id; N cores × {id, thread exists[2]}}}`, `0xff` = absent.
//! N is 4 on Rome (CCD 0x1b bytes, CCX 0xd) and 8 on Milan (0x33, 0x19);
//! Naples has no such entry (its map is SMN `0x5a81c` per die). The layout
//! is the APOB's, not the silicon's: no part has 2 CCX × 8 cores. ApobSspPei
//! `ffcfa1ba` (Milan), decompiler-only; matches Oxide's public structures.

use heapless::Vec;
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

use super::{Apob, ApobError, group, instance};
use crate::sp3::soc::Generation;

/// Absent CCD/CCX/core id.
pub const ABSENT: u8 = 0xff;
const KIND: u32 = 3;
const CCDS: usize = 8;
const CCXS: usize = 2;

#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct RawCore {
    id: u8,
    threads: [u8; 2],
}

#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct RawCcx<const N: usize> {
    id: u8,
    cores: [RawCore; N],
}

#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct RawCcd<const N: usize> {
    id: u8,
    ccxs: [RawCcx<N>; CCXS],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Core {
    /// Physical core id within the CCX.
    pub id: u8,
    pub threads: [bool; 2],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ccx {
    /// Physical CCX id within the CCD.
    pub id: u8,
    pub cores: Vec<Core, 8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ccd {
    /// Physical CCD id (the `0x2000000` SMN stride of the CCD registers).
    pub id: u8,
    pub ccxs: Vec<Ccx, CCXS>,
}

/// The present CCDs, CCXs and cores of one socket, in APOB order.
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

    /// `(ccd id, ccx id, core)` for every present core.
    pub fn cores(&self) -> impl Iterator<Item = (u8, u8, &Core)> {
        self.ccds
            .iter()
            .flat_map(|d| d.ccxs.iter().map(move |x| (d.id, x)))
            .flat_map(|(d, x)| x.cores.iter().map(move |c| (d, x.id, c)))
    }

    fn parse<const N: usize>(raw: &[RawCcd<N>; CCDS]) -> Self {
        let present = |id: &u8| *id != ABSENT;
        // Capacities equal the record's array lengths: pushes cannot fail.
        let ccds = raw.iter().filter(|d| present(&d.id)).map(|d| Ccd {
            id: d.id,
            ccxs: d
                .ccxs
                .iter()
                .filter(|x| present(&x.id))
                .map(|x| Ccx {
                    id: x.id,
                    cores: x
                        .cores
                        .iter()
                        .filter(|c| present(&c.id))
                        .map(|c| Core {
                            id: c.id,
                            threads: c.threads.map(|t| t != 0),
                        })
                        .collect(),
                })
                .collect(),
        });
        Self {
            ccds: ccds.collect(),
        }
    }
}

/// The 3/3 map of `socket`.
pub fn core_map(apob: &Apob<'_>, socket: u8) -> Result<CoreMap, ApobError> {
    apob.require(group::CCX, KIND, &[Generation::Rome, Generation::Milan])?;
    let entry = apob.find(group::CCX, KIND, instance(socket, 0))?;
    Ok(match apob.generation() {
        Generation::Milan => CoreMap::parse::<8>(entry.at(0x30)?),
        _ => CoreMap::parse::<4>(entry.at(0x30)?),
    })
}

#[cfg(test)]
mod tests {
    use super::super::build::Builder;
    use super::*;

    /// A 3/3 body: CCDs `ccds` present with one CCX of `cores` SMT cores,
    /// the rest absent.
    fn body(per_ccx: usize, ccds: &[u8], cores: usize) -> std::vec::Vec<u8> {
        let ccx = |id: u8, n: usize| {
            let mut v = vec![id];
            (0..per_ccx).for_each(|c| {
                v.extend(if c < n {
                    [c as u8, 1, 1]
                } else {
                    [ABSENT, 0, 0]
                })
            });
            v
        };
        (0..8u8)
            .flat_map(|d| {
                let on = ccds.contains(&d);
                let mut v = vec![if on { d } else { ABSENT }];
                v.extend(ccx(if on { 0 } else { ABSENT }, if on { cores } else { 0 }));
                v.extend(ccx(ABSENT, 0));
                v
            })
            .collect()
    }

    #[test]
    fn strides() {
        assert_eq!(size_of::<RawCcx<4>>(), 0xd);
        assert_eq!(size_of::<RawCcd<4>>(), 0x1b);
        assert_eq!(size_of::<RawCcx<8>>(), 0x19);
        assert_eq!(size_of::<RawCcd<8>>(), 0x33);
    }

    #[test]
    fn milan_8_cores_per_ccx() {
        let data = Builder::new(0x18)
            .entry(3, 3, 0x100, &body(8, &[0, 2, 5], 8))
            .build();
        let apob = Apob::new(&data, Generation::Milan).unwrap();
        let map = core_map(&apob, 1).unwrap();
        assert_eq!(
            map.ccds.iter().map(|d| d.id).collect::<std::vec::Vec<_>>(),
            [0, 2, 5]
        );
        assert_eq!(map.ccds[1].ccxs.len(), 1);
        assert_eq!(map.ccds[1].ccxs[0].cores.len(), 8);
        assert_eq!(map.threads(), 48);
        assert!(core_map(&apob, 0).is_err());
    }

    #[test]
    fn rome_4_cores_per_ccx() {
        let data = Builder::new(0x18)
            .entry(3, 3, 0, &body(4, &[1, 3], 3))
            .build();
        let apob = Apob::new(&data, Generation::Rome).unwrap();
        let map = core_map(&apob, 0).unwrap();
        assert_eq!(map.ccds.len(), 2);
        assert_eq!(map.threads(), 12);
        // A Rome-sized entry is too short for the Milan layout.
        let apob = Apob::new(&data, Generation::Milan).unwrap();
        assert_eq!(
            core_map(&apob, 0),
            Err(ApobError::TooShort { group: 3, kind: 3 })
        );
        let apob = Apob::new(&data, Generation::Naples).unwrap();
        assert!(matches!(
            core_map(&apob, 0),
            Err(ApobError::Unsupported { .. })
        ));
    }
}
