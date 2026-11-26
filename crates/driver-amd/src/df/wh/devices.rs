// SPDX-License-Identifier: MIT OR Apache-2.0
//! The WH Data Fabric's per-IOD device tables and PIE RAS instance lists.
//!
//! Generated from SilVenicePei `WhBaseFabricTopology` device tables
//! (`0xfc4ac8`, `0xfc49c8`, `0xfc48c8`, `0xfc47c8`) and the
//! `Df4WhPieRasInit` instance lists (`0xf9f2a0`, `0xf9ed40`, `0xf9ef38`,
//! `0xf9f0a0`) of Dell PR9XJ 1.0.8 (VenicePI-SP7 1.0.0.0, SilVenicePei
//! sha256 `6a838139f1b5ef20…`); every table is byte-identical in GIGABYTE
//! MD25-CG0 F01 (SilVenicePei `e878d40a6ac60906…`). Generator:
//! `tools/fabric2p_tables.py` (research tree, not part of this one).
//!
//! The device table is chosen by CPUID (`0xf2be40`,
//! [`crate::cpu::wh::Part::devices`]); which PIE list goes with
//! which table is matched by the instance ranges each list covers.

/// A run of fabric instances of one device type on an IOD.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Block {
    /// First instance ID.
    pub first: u8,
    /// Most instances of the type.
    pub count: u8,
}

impl Block {
    /// The instance IDs of the block.
    pub fn instances(self) -> impl Iterator<Item = u8> {
        (0..self.count).map(move |i| self.first + i)
    }

    /// Index of `instance` within the block.
    pub const fn index(self, instance: u8) -> Option<u8> {
        if instance >= self.first && instance - self.first < self.count {
            Some(instance - self.first)
        } else {
            None
        }
    }
}

/// The device table of one WH IOD.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Devices {
    /// The device table's address in the Dell donor (its name).
    pub table: u32,
    /// UMC chip selects (type `0x6`): two per memory channel.
    pub cs: Block,
    /// IOMs of type `0xD`; the first is the DRAM map copy the vendor's NUMA
    /// code reads.
    pub iom: Block,
    /// IOMs of type `0x14` (the B0 MMIO-gap patch also writes these).
    pub iom_ext: Block,
    /// IOS (type `0xE`): one root bridge each. The first is `Ios0`, the IOS
    /// of the IOD's FCH and SMU; a root bridge's physical index is its IOS
    /// instance minus `Ios0`.
    pub ios: Block,
    /// The instances `Df4WhPieRasInit` sets the watchdog of, as inclusive
    /// runs in the vendor's order.
    pub pie: &'static [(u8, u8)],
}

/// SP7 B0 and later (`0x00B5_0Fxx`/`0x00BC_0Fxx`, model ≤ 7, stepping ≠ 0).
pub const B0: Devices = Devices {
    table: 0xfc4ac8,
    cs: Block {
        first: 0x00,
        count: 16,
    },
    iom: Block {
        first: 0x23,
        count: 3,
    },
    iom_ext: Block {
        first: 0x22,
        count: 1,
    },
    ios: Block {
        first: 0x26,
        count: 3,
    },
    pie: &[(0x00, 0x2e), (0x34, 0x37), (0x32, 0x32), (0x38, 0x80)],
};

/// SP7 A0 (`0x00B5_0F00`, stepping 0 exactly).
pub const A0: Devices = Devices {
    table: 0xfc49c8,
    cs: Block {
        first: 0x00,
        count: 16,
    },
    iom: Block {
        first: 0x24,
        count: 2,
    },
    iom_ext: Block {
        first: 0x22,
        count: 2,
    },
    ios: Block {
        first: 0x26,
        count: 2,
    },
    pie: &[(0x00, 0x7b)],
};

/// `0x00B9_0Fxx`/`0x00BA_0Fxx` (four IOS per IOD).
pub const B9: Devices = Devices {
    table: 0xfc48c8,
    cs: Block {
        first: 0x00,
        count: 8,
    },
    iom: Block {
        first: 0x1b,
        count: 4,
    },
    iom_ext: Block {
        first: 0x1a,
        count: 1,
    },
    ios: Block {
        first: 0x1f,
        count: 4,
    },
    pie: &[(0x00, 0x2a), (0x32, 0x32), (0x34, 0x60)],
};

/// `0x00B5_0F8x`/`0x00BC_0F8x` (model ≥ 8) in package type `0xD`.
pub const M8: Devices = Devices {
    table: 0xfc47c8,
    cs: Block {
        first: 0x00,
        count: 16,
    },
    iom: Block {
        first: 0x23,
        count: 3,
    },
    iom_ext: Block {
        first: 0x22,
        count: 1,
    },
    ios: Block {
        first: 0x26,
        count: 3,
    },
    pie: &[(0x00, 0x2e), (0x34, 0x37), (0x32, 0x32), (0x38, 0x82)],
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ios_and_pie_runs_match_the_tables() {
        assert_eq!(B0.ios.index(0x28), Some(2));
        assert_eq!(B0.ios.index(0x29), None);
        assert_eq!(
            A0.ios.instances().collect::<std::vec::Vec<_>>(),
            [0x26, 0x27]
        );
        let len = |d: &Devices| {
            d.pie
                .iter()
                .map(|&(a, b)| (b - a) as usize + 1)
                .sum::<usize>()
        };
        // The donor lists' lengths.
        assert_eq!(
            [len(&B0), len(&A0), len(&B9), len(&M8)],
            [125, 124, 89, 127]
        );
    }
}
