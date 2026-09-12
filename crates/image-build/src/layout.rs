//! Host encoder for the stage-local layout projection.
//!
//! Call this only after the build resolver has assigned fixed reservations.
//! Encoding validates the wire contract through the same decoder as firmware;
//! placement/overlap policy belongs to the resolver, not this transport.

use fstart_core::layout::{
    Error, HEADER_LEN, Layout, MAGIC, MAX_REGIONS, REGION_LEN, Region, VERSION,
};

/// Immutable wire bytes, ready for linker BYTE emission.
#[derive(Debug, Clone)]
pub struct EncodedLayout(Vec<u8>);

impl EncodedLayout {
    pub fn encode(stage_index: u16, regions: &[Region]) -> Result<Self, Error> {
        if !(1..=MAX_REGIONS).contains(&regions.len()) {
            return Err(Error::InvalidCount);
        }
        // The count bound makes allocation and the u16 encoded length safe.
        let len = HEADER_LEN + regions.len() * REGION_LEN;
        let mut bytes = Vec::with_capacity(len);
        bytes.extend_from_slice(&MAGIC);
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.extend_from_slice(&(len as u16).to_le_bytes());
        bytes.extend_from_slice(&stage_index.to_le_bytes());
        bytes.extend_from_slice(&(regions.len() as u16).to_le_bytes());
        bytes.extend_from_slice(&[0; 4]);
        for region in regions {
            bytes.extend_from_slice(&(region.kind as u16).to_le_bytes());
            bytes.extend_from_slice(&[0; 6]);
            bytes.extend_from_slice(&region.base.to_le_bytes());
            bytes.extend_from_slice(&region.size.to_le_bytes());
        }
        Layout::parse(&bytes)?;
        Ok(Self(bytes))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_core::layout::{MAX_ENCODED_LEN, RegionKind};

    const FLASH: Region = Region {
        kind: RegionKind::Flash,
        base: 0x2000_0000,
        size: 0x0200_0000,
    };

    #[test]
    fn encoder_agrees_with_independent_wire_fixture() {
        let encoded = EncodedLayout::encode(7, &[FLASH]).unwrap();
        // Do not derive expected bytes using the encoder or native struct layout.
        assert_eq!(
            encoded.as_bytes(),
            &[
                b'F', b'S', b'L', b'Y', 1, 0, 40, 0, 7, 0, 1, 0, 0, 0, 0, 0, 5, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0x20, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0,
            ]
        );
    }

    #[test]
    fn round_trips_all_roles_and_addresses_above_four_gib() {
        let regions = [
            FLASH,
            Region {
                kind: RegionKind::Image,
                base: FLASH.base,
                size: 0x100000,
            },
            Region {
                kind: RegionKind::Writable,
                base: 0x1_8000_0000,
                size: 0x100000,
            },
            Region {
                kind: RegionKind::Stack,
                base: 0x1_800f_0000,
                size: 0x10000,
            },
            Region {
                kind: RegionKind::Heap,
                base: 0x1_8008_0000,
                size: 0x70000,
            },
            Region {
                kind: RegionKind::Reserved,
                base: 0x8000_0000,
                size: 0x1000,
            },
            Region {
                kind: RegionKind::Reserved,
                base: 0x9000_0000,
                size: 0x1000,
            },
        ];
        let encoded = EncodedLayout::encode(u16::MAX, &regions).unwrap();
        let view = Layout::parse(encoded.as_bytes()).unwrap();
        assert_eq!(view.stage_index(), u16::MAX);
        assert_eq!(view.regions().collect::<Vec<_>>(), regions);
    }

    #[test]
    fn bounds_counts_and_rejects_invalid_regions() {
        assert_eq!(
            EncodedLayout::encode(0, &[]).unwrap_err(),
            Error::InvalidCount
        );
        let reserved = Region {
            kind: RegionKind::Reserved,
            ..FLASH
        };
        let regions = [reserved; MAX_REGIONS];
        let encoded = EncodedLayout::encode(0, &regions).unwrap();
        assert_eq!(encoded.as_bytes().len(), MAX_ENCODED_LEN);
        assert_eq!(
            Layout::parse(encoded.as_bytes()).unwrap().regions().len(),
            MAX_REGIONS
        );
        assert_eq!(
            EncodedLayout::encode(0, &[reserved; MAX_REGIONS + 1]).unwrap_err(),
            Error::InvalidCount,
        );
        for kind in [
            RegionKind::Image,
            RegionKind::Writable,
            RegionKind::Stack,
            RegionKind::Heap,
            RegionKind::Flash,
        ] {
            let region = Region { kind, ..FLASH };
            assert_eq!(
                EncodedLayout::encode(0, &[region, region]).unwrap_err(),
                Error::DuplicateRegionKind
            );
        }
        for region in [
            Region { size: 0, ..FLASH },
            Region {
                base: u64::MAX,
                ..FLASH
            },
        ] {
            assert_eq!(
                EncodedLayout::encode(0, &[region]).unwrap_err(),
                Error::InvalidRange
            );
        }
    }
}
