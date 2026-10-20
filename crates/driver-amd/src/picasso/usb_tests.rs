// SPDX-License-Identifier: MIT OR Apache-2.0
extern crate std;
use crate::ccx::tsc::testing::Delay;
use super::usb;
use crate::ccx::picasso::Identity;
use crate::smu::v10 as smu;
use crate::smn::testing::{SmnModel, SmnTest};
use std::{collections::BTreeMap, vec::Vec};
struct Transport {
    regs: BTreeMap<u32, u32>,
    writes: Vec<(u32, u32)>,
    frames: Vec<(u32, [u32; 6])>,
    response: u32,
    reject: Option<usize>,
}
impl Transport {
    fn new() -> Self {
        Self {
            regs: [(0x03100000, 0x80), (0x16d8010c, 0xff), (0x16f8010c, 0xff)].into(),
            writes: Vec::new(),
            frames: Vec::new(),
            response: 1,
            reject: None,
        }
    }
}
impl SmnModel for Transport {
    fn read(&mut self, address: u32) -> u32 {
        if address == smu::RESPONSE.address() {
            self.response
        } else {
            self.regs.get(&address).copied().unwrap_or(0)
        }
    }
    fn write(&mut self, address: u32, value: u32) {
        self.regs.insert(address, value);
        self.writes.push((address, value));
        if address == smu::RESPONSE.address() {
            self.response = value;
        }
        if address == smu::MESSAGE.address() {
            self.frames.push((
                value,
                core::array::from_fn(|i| self.regs[&(smu::ARGUMENTS.address() + i as u32 * 4)]),
            ));
            self.response = if self.reject == Some(self.frames.len()) {
                0xfe
            } else {
                1
            };
        }
    }
}
struct Phy {
    data: [u32; 8],
    writes: Vec<(usize, u32)>,
}
impl Phy {
    fn new() -> Self {
        Self {
            data: [0xff; 8],
            writes: Vec::new(),
        }
    }
}
impl usb::PhyRegisters for Phy {
    fn read(&mut self, i: usize) -> u32 {
        self.data[i]
    }
    fn write(&mut self, i: usize, value: u32) {
        self.data[i] = value;
        self.writes.push((i, value));
    }
}
fn config() -> usb::Config {
    usb::Config {
        phy2: None,
        phy3_override: None,
        overcurrent: [
            usb::Overcurrent::Pin0,
            usb::Overcurrent::None,
            usb::Overcurrent::Pin0,
            usb::Overcurrent::Pin1,
            usb::Overcurrent::None,
            usb::Overcurrent::None,
        ],
        force_gen1: 0,
    }
}
#[test]
fn cold_and_resume_transactions_use_exact_groups_then_completion() {
    let id = Identity::from_cpuid(0x820f00, 0, 1 << 30).unwrap();
    for resume in [false, true] {
        let test = SmnTest::new(Transport::new());
        let mut phy = Phy::new();
        usb::initialize(
            &test.smn(),
            &mut phy,
            &config(),
            id,
            1 << 30,
            resume,
            &mut Delay(|_| {}),
        )
        .unwrap();
        let bus = test.model();
        assert_eq!(
            &bus.writes[..3],
            &[(0x03100004, 0), (0x03100000, 0x81), (0x03100000, 0x881)]
        );
        assert_eq!(
            phy.data,
            [0x101ff, 0x9393, 0x303, 0, 0x101ff, 0x9393, 0x303, 0]
        );
        assert_eq!(phy.writes.len(), 8);
        assert_eq!(bus.frames.len(), 32);
        assert_eq!(bus.frames[0], (0x44, [0; 6]));
        assert_eq!(
            bus.frames[1],
            (0x38, [0x16d80128, 0xf003f, 0, 0x01000001, 0, 0])
        );
        assert_eq!(
            bus.frames[2],
            (0x38, [0x16d80120, u32::MAX, 0xf0ff10f0, 0x01000001, 0, 0])
        );
        assert_eq!(
            bus.frames[3],
            (0x38, [0x16d80124, 0xff, 0x10, 0x01000001, 0, 0])
        );
        assert_eq!(bus.frames[18], (0x38, [0, 0, 0, 0x8200000f, 0, 0]));
        assert_eq!(
            bus.frames.last().copied(),
            Some((if resume { 0x2d } else { 0x2e }, [0; 6]))
        );
        assert_eq!(bus.regs[&0x16d8010c], 0xbf);
        assert_eq!(bus.regs.contains_key(&0x16d80104), !resume);
        assert_eq!(bus.regs.contains_key(&0x16f80104), !resume);
    }
}
#[test]
fn rejected_disable_stops_before_phy_and_invalid_tuning_never_writes() {
    let id = Identity::from_cpuid(0x820f00, 0, 1 << 30).unwrap();
    let test = SmnTest::new(Transport::new());
    test.model().reject = Some(1);
    let mut phy = Phy::new();
    assert_eq!(
        usb::initialize(
            &test.smn(),
            &mut phy,
            &config(),
            id,
            1 << 30,
            false,
            &mut Delay(|_| {}
        )),
        Err(usb::Error::Smu(smu::Error::Rejected(0xfe)))
    );
    assert_eq!(test.model().frames.len(), 1);
    assert!(phy.writes.is_empty());
    let test = SmnTest::new(Transport::new());
    let mut invalid = config();
    invalid.phy2 = Some(
        [usb::Phy2 {
            vref: 16,
            ..usb::Phy2::DEFAULT
        }; 6],
    );
    assert_eq!(
        usb::initialize(
            &test.smn(),
            &mut phy,
            &invalid,
            id,
            1 << 30,
            false,
            &mut Delay(|_| {}
        )),
        Err(usb::Error::Configuration)
    );
    assert!(test.model().writes.is_empty());
    assert!(phy.writes.is_empty());
}
