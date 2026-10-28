//! Milan APOB 9/0x15: optional global DXIO PHY overrides.
//!
//! RXM100CB AmdNbioPciePei `ffd2244c` reads instance 0: payload byte count
//! at entry +0x30, up to 0x100 bytes at +0x34 (record size 0x134). Its
//! builder `ffd264b4` forwards these bytes as ancillary type 5, format 1.
//! The payload is opaque; it is not the board's per-engine PHY tuning.

use zerocopy::little_endian::U32;

use super::{Apob, ApobError, group};
use crate::psp::Variant;

const PHY_OVERRIDE: u32 = 0x15;
const MAX_PAYLOAD: usize = 0x100;

/// Instance 0's global payload, shared by the socket DXIO units. Missing
/// data is optional; a present zero-length payload is returned as `Some(&[])`.
/// Reject malformed, duplicate, oversized or non-dword-aligned records;
/// never truncate or fabricate PHY settings. Only Milan's layout is verified.
pub fn phy_override<'a>(apob: &Apob<'a>) -> Result<Option<&'a [u8]>, ApobError> {
    apob.require(group::FABRIC, PHY_OVERRIDE, &[Variant::Gn])?;
    let entry = match apob.find(group::FABRIC, PHY_OVERRIDE, 0) {
        Ok(entry) => entry,
        Err(ApobError::NotFound { .. }) => return Ok(None),
        Err(e) => return Err(e),
    };
    let count = entry.at::<U32>(0x30)?.get();
    if count as usize > MAX_PAYLOAD || count % 4 != 0 {
        return Err(ApobError::Value {
            group: group::FABRIC,
            kind: PHY_OVERRIDE,
            value: count,
        });
    }
    entry.slice::<u8>(0x34, count as usize).map(Some)
}

#[cfg(test)]
mod tests {
    use super::super::build::{Body, Builder};
    use super::*;

    fn record(count: u32, payload: &[u8]) -> Vec<u8> {
        Body::default()
            .at(0x30, &count.to_le_bytes())
            .at(0x34, payload)
            .0
    }

    #[test]
    fn optional_global_payload() {
        let data = Builder::new(0x18).entry(7, 6, 0, &[]).build();
        assert_eq!(
            phy_override(&Apob::new(&data, Variant::Gn).unwrap()),
            Ok(None)
        );
        for len in [0, 4, MAX_PAYLOAD] {
            let payload = vec![0xa5; len];
            let data = Builder::new(0x18)
                .entry(9, 0x15, 0, &record(len as u32, &payload))
                // Nonzero instances do not replace the global instance.
                .entry(9, 0x15, 0x100, &record(4, &[0xff; 4]))
                .build();
            assert_eq!(
                phy_override(&Apob::new(&data, Variant::Gn).unwrap()),
                Ok(Some(payload.as_slice()))
            );
        }
    }

    #[test]
    fn rejects_invalid_counts_and_truncated_records() {
        for count in [1, 3, 0x104, u32::MAX] {
            let data = Builder::new(0x18)
                .entry(9, 0x15, 0, &record(count, &[]))
                .build();
            assert!(matches!(
                phy_override(&Apob::new(&data, Variant::Gn).unwrap()),
                Err(ApobError::Value { value, .. }) if value == count
            ));
        }
        for body in [vec![], record(4, &[]), record(8, &[1; 4])] {
            let data = Builder::new(0x18).entry(9, 0x15, 0, &body).build();
            assert!(matches!(
                phy_override(&Apob::new(&data, Variant::Gn).unwrap()),
                Err(ApobError::TooShort { .. })
            ));
        }
    }

    #[test]
    fn rejects_duplicates_malformed_walk_and_other_variants() {
        let body = record(0, &[]);
        let data = Builder::new(0x18)
            .entry(9, 0x15, 0, &body)
            .entry(9, 0x15, 0, &body)
            .build();
        assert!(matches!(
            phy_override(&Apob::new(&data, Variant::Gn).unwrap()),
            Err(ApobError::Duplicate { .. })
        ));
        let valid = Builder::new(0x18).entry(9, 0x15, 0, &body).build();
        let mut data = valid.clone();
        // Corrupt the entry size, not the APOB header.
        data[0x1c..0x20].copy_from_slice(&0u32.to_le_bytes());
        assert!(matches!(
            Apob::new(&data, Variant::Gn),
            Err(ApobError::Entry { .. })
        ));
        for variant in [Variant::Zp, Variant::Rv, Variant::Ssp] {
            assert!(matches!(
                phy_override(&Apob::new(&valid, variant).unwrap()),
                Err(ApobError::Unsupported { .. })
            ));
        }
    }
}
