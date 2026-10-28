//! APOB 9/0x13 and 9/0x14: NUMA inputs for SRAT (PSP_APOB §3.5;
//! AmdFabricSspDxe `0x163c`, decompiler-only). Rome and Milan only.

use zerocopy::little_endian::{U32, U64};
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

use super::Variant;
use super::{Apob, ApobError, group};

const NPS: u32 = 0x13;
const DOMAINS: u32 = 0x14;
const SP3: &[Variant] = &[Variant::Ssp, Variant::Gn];

/// NUMA nodes per socket (`+0x31`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nps {
    /// One domain for all sockets.
    Nps0,
    Nps1,
    Nps2,
    Nps4,
}

impl Nps {
    /// Domains per socket; `None` for NPS0 (one for the whole system).
    pub const fn domains_per_socket(self) -> Option<u8> {
        match self {
            Self::Nps0 => None,
            Self::Nps1 => Some(1),
            Self::Nps2 => Some(2),
            Self::Nps4 => Some(4),
        }
    }
}

/// Read 9/0x13 instance 0.
pub fn nps(apob: &Apob<'_>) -> Result<Nps, ApobError> {
    apob.require(group::FABRIC, NPS, SP3)?;
    match apob.find(group::FABRIC, NPS, 0)?.byte(0x31)? {
        0 => Ok(Nps::Nps0),
        1 => Ok(Nps::Nps1),
        2 => Ok(Nps::Nps2),
        3 => Ok(Nps::Nps4),
        v => Err(ApobError::Value {
            group: group::FABRIC,
            kind: NPS,
            value: v.into(),
        }),
    }
}

/// A 9/0x14 record.
#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
pub struct RawDomain {
    base: U64,
    /// Checked to be 0 by AGESA.
    zero: U32,
    socket: u8,
    flags: u8,
    _rest: [u8; 10],
}

/// An extra NUMA domain record (meaning of `flags` not traced).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Domain {
    pub base: u64,
    pub socket: u8,
    pub flags: u8,
}

/// The four 9/0x14 records at `+0x38`, skipping those whose reserved dword
/// is not 0 (AGESA's check).
pub fn extra_domains<'a>(apob: &Apob<'a>) -> Result<impl Iterator<Item = Domain> + 'a, ApobError> {
    apob.require(group::FABRIC, DOMAINS, SP3)?;
    let records: &[RawDomain] = apob.find(group::FABRIC, DOMAINS, 0)?.slice(0x38, 4)?;
    Ok(records
        .iter()
        .filter(|r| r.zero.get() == 0)
        .map(|r| Domain {
            base: r.base.get(),
            socket: r.socket,
            flags: r.flags,
        }))
}

#[cfg(test)]
mod tests {
    use super::super::build::{Body, Builder};
    use super::*;

    #[test]
    fn nps_modes() {
        let apob_with = |mode: u8| {
            Builder::new(0x18)
                .entry(9, 0x13, 0, &Body::default().at(0x31, &[mode]).0)
                .build()
        };
        let data = apob_with(3);
        assert_eq!(nps(&Apob::new(&data, Variant::Gn).unwrap()), Ok(Nps::Nps4));
        assert_eq!(Nps::Nps4.domains_per_socket(), Some(4));
        let data = apob_with(4);
        assert!(matches!(
            nps(&Apob::new(&data, Variant::Ssp).unwrap()),
            Err(ApobError::Value { value: 4, .. })
        ));
        let data = apob_with(1);
        assert!(matches!(
            nps(&Apob::new(&data, Variant::Zp).unwrap()),
            Err(ApobError::Unsupported { .. })
        ));
    }

    #[test]
    fn domains() {
        let rec = |base: u64, zero: u32, socket: u8| {
            [
                &base.to_le_bytes()[..],
                &zero.to_le_bytes(),
                &[socket, 1],
                &[0; 10],
            ]
            .concat()
        };
        let recs = [
            rec(0x100_0000_0000, 0, 1),
            rec(0, 5, 0),
            rec(0x80_0000_0000, 0, 0),
            rec(0, 1, 0),
        ]
        .concat();
        let data = Builder::new(0x18)
            .entry(9, 0x14, 0, &Body::default().at(0x38, &recs).0)
            .build();
        let apob = Apob::new(&data, Variant::Ssp).unwrap();
        let d: Vec<_> = extra_domains(&apob).unwrap().collect();
        assert_eq!(
            d,
            [
                Domain {
                    base: 0x100_0000_0000,
                    socket: 1,
                    flags: 1
                },
                Domain {
                    base: 0x80_0000_0000,
                    socket: 0,
                    flags: 1
                },
            ]
        );
    }
}
