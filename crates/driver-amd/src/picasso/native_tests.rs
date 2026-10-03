// SPDX-License-Identifier: MIT OR Apache-2.0
extern crate std;
use super::{
    apob::{self, Apob},
    fabric,
    registers::{BROADCAST, Fabric, FabricPort, Smn, SmnPort},
    smu,
};
use fstart_pci::{ConfigRegionAccess, PciAddress};
use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
    vec,
    vec::Vec,
};

#[derive(Debug, PartialEq, Eq)]
enum PciOp {
    Read(PciAddress, u16),
    Write(PciAddress, u16, u32),
}
#[derive(Default)]
struct Pci {
    ops: RefCell<Vec<PciOp>>,
}
impl ConfigRegionAccess for Pci {
    unsafe fn read(&self, a: PciAddress, o: u16) -> u32 {
        self.ops.borrow_mut().push(PciOp::Read(a, o));
        0x12345678
    }
    unsafe fn write(&self, a: PciAddress, o: u16, v: u32) {
        self.ops.borrow_mut().push(PciOp::Write(a, o, v));
    }
}
#[test]
fn indirect_register_targets_and_broadcast_are_distinct() {
    let pci = Pci::default();
    let mut smn = unsafe { SmnPort::new(&pci) };
    assert_eq!(smn.read(0x3b10564), 0x12345678);
    let mut df = unsafe { FabricPort::new(&pci) };
    df.write(5, 0x348, 9, 0x14171710);
    df.write(5, 0x348, BROADCAST, 0x14171710);
    assert_eq!(
        *pci.ops.borrow(),
        vec![
            PciOp::Write(PciAddress::new(0, 0, 0, 0), 0xb8, 0x3b10564),
            PciOp::Read(PciAddress::new(0, 0, 0, 0), 0xbc),
            PciOp::Write(
                PciAddress::new(0, 0, 0x18, 4),
                0x5c,
                (9 << 16) | (5 << 11) | 0x348 | 1
            ),
            PciOp::Write(PciAddress::new(0, 0, 0x18, 4), 0x98, 0x14171710),
            PciOp::Write(PciAddress::new(0, 0, 0x18, 5), 0x348, 0x14171710),
        ]
    );
}

#[derive(Default)]
struct Mailbox {
    responses: VecDeque<u32>,
    writes: Vec<(u32, u32)>,
}
impl Smn for Mailbox {
    fn read(&mut self, address: u32) -> u32 {
        if address == smu::RESPONSE {
            self.responses.pop_front().unwrap_or(0)
        } else {
            0x100 + (address - smu::ARGUMENTS) / 4
        }
    }
    fn write(&mut self, address: u32, value: u32) {
        self.writes.push((address, value));
    }
}
#[test]
fn smu_command_is_last_and_previous_error_does_not_block_readiness() {
    let mut mailbox = Mailbox {
        responses: [0xff, 0, 1].into(),
        ..Default::default()
    };
    let mut elapsed = 0;
    let result = smu::request(
        &mut mailbox,
        smu::GET_SILICON_ID,
        [1, 2, 3, 4, 5, 6],
        &mut |us| elapsed += us,
    )
    .unwrap();
    assert_eq!(result, [0x100, 0x101, 0x102, 0x103, 0x104, 0x105]);
    assert_eq!(elapsed, 100);
    assert_eq!(
        mailbox.writes,
        vec![
            (smu::RESPONSE, 0),
            (smu::ARGUMENTS, 1),
            (smu::ARGUMENTS + 4, 2),
            (smu::ARGUMENTS + 8, 3),
            (smu::ARGUMENTS + 12, 4),
            (smu::ARGUMENTS + 16, 5),
            (smu::ARGUMENTS + 20, 6),
            (smu::MESSAGE, smu::GET_SILICON_ID),
        ]
    );
}
#[test]
fn raw_smu_reply_retains_output_context_without_treating_status_as_success() {
    let mut mailbox = Mailbox {
        responses: [1, 0xfe].into(),
        ..Default::default()
    };
    let reply = smu::request_raw(&mut mailbox, 0x13, [0; 6], &mut |_| {}).unwrap();
    assert_eq!(reply.status, 0xfe);
    assert_eq!(reply.arguments, [0x100, 0x101, 0x102, 0x103, 0x104, 0x105]);
    mailbox.responses = [1, u32::MAX].into();
    assert_eq!(
        smu::request_raw(&mut mailbox, 0x13, [0; 6], &mut |_| {}),
        Err(smu::Error::Unavailable)
    );
    let mut unavailable = Mailbox {
        responses: [u32::MAX, 1].into(),
        ..Default::default()
    };
    assert_eq!(
        smu::request_raw(&mut unavailable, 0x13, [0; 6], &mut |_| {}),
        Err(smu::Error::Unavailable)
    );
    assert!(unavailable.writes.is_empty());
}

#[test]
fn smu_timeout_is_bounded_and_never_submits_when_busy() {
    let mut mailbox = Mailbox::default();
    let mut elapsed = 0;
    assert_eq!(
        smu::request(&mut mailbox, 1, [0; 6], &mut |us| elapsed += us),
        Err(smu::Error::BusyTimeout)
    );
    assert_eq!(elapsed, 10_000_000);
    assert!(mailbox.writes.is_empty());
    mailbox.responses = [1, 0xfe].into();
    assert_eq!(
        smu::request(&mut mailbox, 1, [0; 6], &mut |_| {}),
        Err(smu::Error::Rejected(0xfe))
    );
}

#[derive(Default)]
struct Df {
    registers: BTreeMap<(u8, u16, u8), u32>,
    writes: Vec<(u8, u16, u8, u32)>,
}
impl Fabric for Df {
    fn read(&mut self, f: u8, o: u16, i: u8) -> u32 {
        *self.registers.get(&(f, o, i)).unwrap_or(&0)
    }
    fn write(&mut self, f: u8, o: u16, i: u8, v: u32) {
        self.registers.insert((f, o, i), v);
        self.writes.push((f, o, i, v));
    }
}
#[test]
fn raven2_late_df_clears_bit11_after_setting_power_down_policy() {
    for raven2 in [false, true] {
        for old in [0, 0x80000020, 0x80000820] {
            let mut df = Df::default();
            df.registers.insert((5, 0x300, BROADCAST), old);
            fabric::late_init(&mut df, raven2);
            let expected =
                ((old & !(1 << 5)) | 0x3b00) & if raven2 { !(1 << 11) } else { u32::MAX };
            assert_eq!(df.read(5, 0x300, BROADCAST), expected);
            assert_eq!(df.writes[0], (5, 0x300, BROADCAST, expected));
        }
    }
}

#[test]
fn fabric_route_validation_is_atomic_and_programming_is_idempotent() {
    let mut df = Df::default();
    for i in 0..8 {
        df.registers.insert((0, 0xc0 + i * 8, BROADCAST), 3);
    }
    assert_eq!(
        fabric::ensure_legacy_routes(&mut df, 0xd0000000),
        Err(fabric::Error::NoIoWindow)
    );
    assert!(df.writes.is_empty());
    df.registers.clear();
    fabric::ensure_legacy_routes(&mut df, 0xd0000000).unwrap();
    assert_eq!(
        df.writes,
        vec![
            (0, 0x208, BROADCAST, 0),
            (0, 0x200, BROADCAST, 0xd000),
            (0, 0x204, BROADCAST, 0xfebf),
            (0, 0x208, BROADCAST, 0x93),
            (0, 0xc4, BROADCAST, 0xf009),
            (0, 0xc0, BROADCAST, 3),
            (0, 0x80, BROADCAST, 0x91),
        ]
    );
    df.writes.clear();
    fabric::ensure_legacy_routes(&mut df, 0xd0000000).unwrap();
    assert_eq!(df.writes, vec![(0, 0x80, BROADCAST, 0x91)]);
}

fn apob_fixture() -> Vec<u8> {
    let mut bytes = vec![0; 16 + 48 + 16 + 48];
    let len = bytes.len() as u32;
    bytes[0..4].copy_from_slice(b"APOB");
    bytes[4..8].copy_from_slice(&2u32.to_le_bytes());
    bytes[8..12].copy_from_slice(&len.to_le_bytes());
    bytes[12..16].copy_from_slice(&16u32.to_le_bytes());
    bytes[16..20].copy_from_slice(&9u32.to_le_bytes());
    bytes[20..24].copy_from_slice(&9u32.to_le_bytes());
    bytes[28..32].copy_from_slice(&(len - 16).to_le_bytes());
    bytes[64..72].copy_from_slice(&(16u64 << 30).to_le_bytes());
    bytes[72..76].copy_from_slice(&2u32.to_le_bytes());
    for (slot, base, size, kind) in [
        (80, 0xd0000000u64, 0x30000000u64, 1u32),
        (104, 0xc7800000, 0x08000000, 0),
    ] {
        bytes[slot..slot + 8].copy_from_slice(&base.to_le_bytes());
        bytes[slot + 8..slot + 16].copy_from_slice(&size.to_le_bytes());
        bytes[slot + 16..slot + 20].copy_from_slice(&kind.to_le_bytes());
    }
    bytes
}
#[test]
fn apob_memory_map_is_bounded_sorted_and_preserves_uma() {
    let bytes = apob_fixture();
    let apob = Apob::parse(&bytes).unwrap();
    assert_eq!(apob.version(), 2);
    assert_eq!(apob.memory_clock_mhz(), None);
    let map = apob.memory_map().unwrap();
    assert_eq!(map.top, 16 << 30);
    assert_eq!(map.low_top(), 0xc7800000);
    assert_eq!(map.holes()[0].kind, 0);
    assert_eq!(map.holes()[1].kind, 1);
    assert_eq!(map.holes()[0].end(), 0xcf800000);
}
#[test]
fn apob_corruption_never_becomes_a_guessed_memory_map() {
    let mut bytes = apob_fixture();
    bytes[28..32].copy_from_slice(&0u32.to_le_bytes());
    assert!(matches!(Apob::parse(&bytes), Err(apob::Error::Entry)));
    let mut bytes = apob_fixture();
    bytes[72..76].copy_from_slice(&19u32.to_le_bytes());
    assert!(matches!(
        Apob::parse(&bytes).unwrap().memory_map(),
        Err(apob::Error::TooManyHoles)
    ));
    let mut bytes = apob_fixture();
    bytes[80..88].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(matches!(
        Apob::parse(&bytes).unwrap().memory_map(),
        Err(apob::Error::Overflow)
    ));
    let bytes = apob_fixture();
    assert!(Apob::parse(&bytes[..bytes.len() - 1]).is_err());
}
