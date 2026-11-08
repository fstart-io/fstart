// SPDX-License-Identifier: MIT OR Apache-2.0
//! DF4 DRAM address map read-back (the ABL programs it; the x86 only reads):
//! F7xE00 + 10h x rule base/limit/control, F7x104 DramHoleControl. Register
//! geometry: public openSIL DfX SilFabricRegistersDfX.h.
use super::super::DfReg;
use heapless::Vec;
use tock_registers::{LocalRegisterCopy, register_bitfields};

register_bitfields![u32,
    /// Address bits 55:28.
    pub DramAddress [ADDRESS OFFSET(0) NUMBITS(28) []],
    pub DramControl [
        VALID OFFSET(0) NUMBITS(1) [],
        LEGACY_HOLE OFFSET(1) NUMBITS(1) [],
        /// The target CS (the first one of an interleave set).
        DESTINATION OFFSET(16) NUMBITS(12) []
    ],
    pub DramHole [
        VALID OFFSET(0) NUMBITS(1) [],
        BASE OFFSET(24) NUMBITS(8) []
    ]
];
pub const RULES: u8 = 20;
pub const STRIDE: u16 = 0x10;
pub const BASE: DfReg<DramAddress::Register> = DfReg::new(7, 0xe00);
pub const LIMIT: DfReg<DramAddress::Register> = DfReg::new(7, 0xe04);
pub const CONTROL: DfReg<DramControl::Register> = DfReg::new(7, 0xe08);
pub const HOLE: DfReg<DramHole::Register> = DfReg::new(7, 0x104);

/// One valid rule: `[base, end)` and its target CS fabric ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rule {
    pub base: u64,
    pub end: u64,
    pub destination: u16,
    /// The range spans the legacy MMIO hole below 4 GiB.
    pub hole: bool,
}
impl Rule {
    pub fn decode(base: u32, limit: u32, control: u32) -> Option<Self> {
        let control = LocalRegisterCopy::<u32, DramControl::Register>::new(control);
        if control.get() == u32::MAX || !control.is_set(DramControl::VALID) {
            return None;
        }
        let address = |raw| {
            u64::from(
                LocalRegisterCopy::<u32, DramAddress::Register>::new(raw)
                    .read(DramAddress::ADDRESS),
            ) << 28
        };
        Some(Self {
            base: address(base),
            end: address(limit) + (1 << 28),
            destination: control.read(DramControl::DESTINATION) as u16,
            hole: control.is_set(DramControl::LEGACY_HOLE),
        })
    }
}

/// The DRAM rules and the legacy MMIO hole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DramMap {
    pub rules: Vec<Rule, { RULES as usize }>,
    /// DramHoleControl base when valid: a hole rule has no DRAM in
    /// `[hole, 4 GiB)`; that DRAM is hoisted above 4 GiB.
    pub hole_base: Option<u64>,
}
impl DramMap {
    /// Memory `(base, end, destination)` with the hole cut out.
    pub fn ranges(&self) -> impl Iterator<Item = (u64, u64, u16)> + '_ {
        self.rules.iter().flat_map(move |r| {
            let split = self
                .hole_base
                .filter(|&h| r.hole && r.base < h && r.end > 1 << 32);
            let first = match split {
                Some(h) => (r.base, h, r.destination),
                None => (r.base, r.end, r.destination),
            };
            let second = split.map(|_| (1 << 32, r.end, r.destination));
            core::iter::once(first).chain(second)
        })
    }
}

/// The primary IOMS (instance 34), whose copy of the map openSIL reads; a
/// broadcast read merges the instances' differing copies.
pub const READ_INSTANCE: u8 = 34;

/// Read the map from the primary IOMS's copy.
pub fn read<P: fstart_pci::ConfigRegionAccess>(df: &super::Df<P>) -> DramMap {
    use tock_registers::interfaces::Readable;
    let rules = (0..RULES)
        .filter_map(|i| {
            Rule::decode(
                df.instance(BASE.at(i, STRIDE), READ_INSTANCE).get(),
                df.instance(LIMIT.at(i, STRIDE), READ_INSTANCE).get(),
                df.instance(CONTROL.at(i, STRIDE), READ_INSTANCE).get(),
            )
        })
        .collect();
    let hole = df.instance(HOLE, READ_INSTANCE).extract();
    DramMap {
        rules,
        hole_base: hole
            .is_set(DramHole::VALID)
            .then(|| u64::from(hole.read(DramHole::BASE)) << 24),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rules_decode_55_28_and_split_at_the_hole() {
        assert_eq!(Rule::decode(0, 0x1f4, 0x0003_0000), None);
        let low = Rule::decode(0, 0x204, 0x0004_0003).unwrap();
        assert_eq!(
            (low.base, low.end, low.destination, low.hole),
            (0, 0x20_5000_0000, 4, true)
        );
        let map = DramMap {
            rules: Vec::from_slice(&[low, Rule::decode(0x205, 0x404, 0x000a_0001).unwrap()])
                .unwrap(),
            hole_base: Some(0xb000_0000),
        };
        let ranges: std::vec::Vec<_> = map.ranges().collect();
        assert_eq!(
            ranges,
            [
                (0, 0xb000_0000, 4),
                (1 << 32, 0x20_5000_0000, 4),
                (0x20_5000_0000, 0x40_5000_0000, 10)
            ]
        );
    }
}
