// SPDX-License-Identifier: MIT OR Apache-2.0
extern crate std;
use super::super::{
    registers::testing as mch_testing,
    sa_dmi::tests::{Image, root},
};
use super::*;
use fstart_pci::{PciRoot, PciRootInfo};
use std::{boxed::Box, vec, vec::Vec};

pub(crate) struct Rig {
    pub pci: PciEcam,
    _memory: Vec<u32>,
    pub mch: Box<mch_testing::Image>,
    pub dmibar: Box<Image>,
}
impl Rig {
    pub fn new(opi: bool) -> Self {
        Self::with_buses(opi, 2)
    }
    pub fn with_buses(opi: bool, buses: u8) -> Self {
        let mut memory = vec![u32::MAX; usize::from(buses) << 18];
        let pci = PciEcam::new([PciRoot::new(
            PciRootInfo {
                segment: 0,
                ecam_base: memory.as_mut_ptr() as u64,
                bus_start: 0,
                bus_end: buses - 1,
            },
            [],
        )
        .unwrap()])
        .unwrap();
        let sa = root(&pci, opi);
        sa.write32(0xe8, 0);
        sa.write32(0x54, 0);
        sa.write16(0x50, 0x003d);
        Self {
            pci,
            _memory: memory,
            mch: mch_testing::image(),
            dmibar: Image::new(),
        }
    }
    pub fn resources(&self) -> Resources<'_> {
        unsafe { Resources::new(&self.pci, self.mch.registers()) }
    }
    pub fn dev(&self, bus: u8, dev: u8, f: u8) -> EcamDevice {
        self.pci.device(PciAddress::new(0, bus, dev, f)).unwrap()
    }
    pub fn port(&self, f: u8, width: u8) {
        let p = self.dev(0, 1, f);
        for off in (0..0x1000).step_by(4) {
            p.write32(off, 0);
        }
        p.write32(0, 0x1901_8086);
        p.write16(0xa, 0x0604);
        p.write32(0xac, 3 | u32::from(width) << 4);
        p.write16(0xb2, 3 | u16::from(width) << 4);
        p.write16(0x11a, 2);
        p.write8(0xba, 0x40);
        p.write32(0xc24, 1 << 16);
    }
    pub fn endpoint(&self, speed: u8, width: u8) {
        let e = self.dev(1, 0, 0);
        for off in (0..0x1000).step_by(4) {
            e.write32(off, 0);
        }
        e.write32(0, 0x1234_abcd);
        e.write16(6, 0x10);
        e.write8(0x34, 0x40);
        e.write8(0x40, 0x10);
        e.write32(0x4c, u32::from(speed) | u32::from(width) << 4);
    }
}
pub(crate) fn policy() -> Policy {
    Policy {
        devices: Devices {
            peg: [true, false, false],
            igd: true,
            thermal: false,
            imaging: false,
            chap: false,
            gmm: false,
        },
        max_width: [0; 3],
        poll_ms: 2,
        disable_polls: 2,
    }
}

#[test]
fn peg_furcation_lane_reversal_and_unused_x12_bundle_power_are_source_ordered() {
    let rig = Rig::new(false);
    for f in 0..3 {
        rig.port(f, if f == 0 { 8 } else { 4 });
    }
    rig.dev(0, 1, 0).write32(0x504, 0);
    rig.dev(0, 1, 0).write32(0xd0c, 1 << 20);
    let (r, n) = rig.resources().roots().unwrap();
    assert_eq!(n, 3);
    assert_eq!(r[0].bundles(0, 8), 0xf0);
    assert_eq!(r[1].bundles(0, 4), 0x0c);
    assert_eq!(r[2].bundles(0, 4), 0x03);
    let root = Root {
        width: 16,
        start: 0,
        ..Root::default()
    };
    rig.resources().power_bundles(root.bundles(12, 16));
    for b in 0..8 {
        assert_eq!(
            rig.dev(0, 1, 0).read32(0x91c + b * 0x20) >> 31,
            u32::from(b >= 6)
        );
    }
}

#[test]
fn peg_capability_cycle_and_invalid_endpoint_are_bounded_and_restore_bus_numbers() {
    for cycle in [false, true] {
        let rig = Rig::new(false);
        rig.port(0, 16);
        rig.endpoint(3, 16);
        let p = rig.dev(0, 1, 0);
        p.write16(0x11a, 0);
        if cycle {
            rig.dev(1, 0, 0).write8(0x40, 1);
            rig.dev(1, 0, 0).write8(0x41, 0x40);
        } else {
            rig.dev(1, 0, 0).write32(0x4c, 0);
        }
        let mut root = Root {
            width: 16,
            ..Root::default()
        };
        assert!(rig.resources().endpoint(&mut root, policy()).is_err());
        assert_eq!(p.read32(0x18), 0);
    }
}
