// SPDX-License-Identifier: MIT OR Apache-2.0
extern crate std;
use crate::fam17h::tsc::testing::Delay;
use super::{
    apob::{self, Apob},
    fabric,
    registers::{
        Df, DfReg, Smn, SmnReg,
        testing::{DfSpace, SmnModel, SmnTest},
    },
    smu,
};
use tock_registers::interfaces::{Readable, Writeable};
use fstart_pci::{ConfigRegionAccess, PciAddress};
use std::{
    cell::RefCell,
    collections::VecDeque,
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
    let smn = unsafe { Smn::new(&pci) };
    assert_eq!(smn.reg(SmnReg::<()>::new(0x3b10564)).get(), 0x12345678);
    let df = unsafe { Df::new(&pci) };
    let reg = DfReg::<()>::new(5, 0x348);
    df.instance(reg, 9).set(0x14171710);
    df.reg(reg).set(0x14171710);
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
impl SmnModel for Mailbox {
    fn read(&mut self, address: u32) -> u32 {
        if address == smu::RESPONSE.address() {
            self.responses.pop_front().unwrap_or(0)
        } else {
            0x100 + (address - smu::ARGUMENTS.address()) / 4
        }
    }
    fn write(&mut self, address: u32, value: u32) {
        self.writes.push((address, value));
    }
}
#[test]
fn smu_command_is_last_and_previous_error_does_not_block_readiness() {
    let mailbox = SmnTest::new(Mailbox {
        responses: [0xff, 0, 1].into(),
        ..Default::default()
    });
    let mut elapsed = 0;
    let result = smu::request(
        &mailbox.smn(),
        smu::GET_SILICON_ID,
        [1, 2, 3, 4, 5, 6],
        &mut Delay(|us| elapsed += us),
    )
    .unwrap();
    assert_eq!(result, [0x100, 0x101, 0x102, 0x103, 0x104, 0x105]);
    assert_eq!(elapsed, 100);
    assert_eq!(
        mailbox.model().writes,
        vec![
            (smu::RESPONSE.address(), 0),
            (smu::ARGUMENTS.address(), 1),
            (smu::ARGUMENTS.address() + 4, 2),
            (smu::ARGUMENTS.address() + 8, 3),
            (smu::ARGUMENTS.address() + 12, 4),
            (smu::ARGUMENTS.address() + 16, 5),
            (smu::ARGUMENTS.address() + 20, 6),
            (smu::MESSAGE.address(), smu::GET_SILICON_ID),
        ]
    );
}
#[test]
fn raw_smu_reply_retains_output_context_without_treating_status_as_success() {
    let mailbox = SmnTest::new(Mailbox {
        responses: [1, 0xfe].into(),
        ..Default::default()
    });
    let reply = smu::request_raw(&mailbox.smn(), 0x13, [0; 6], &mut Delay(|_| {})).unwrap();
    assert_eq!(reply.status, 0xfe);
    assert_eq!(reply.arguments, [0x100, 0x101, 0x102, 0x103, 0x104, 0x105]);
    mailbox.model().responses = [1, u32::MAX].into();
    assert_eq!(
        smu::request_raw(&mailbox.smn(), 0x13, [0; 6], &mut Delay(|_| {})),
        Err(smu::Error::Unavailable)
    );
    let unavailable = SmnTest::new(Mailbox {
        responses: [u32::MAX, 1].into(),
        ..Default::default()
    });
    assert_eq!(
        smu::request_raw(&unavailable.smn(), 0x13, [0; 6], &mut Delay(|_| {})),
        Err(smu::Error::Unavailable)
    );
    assert!(unavailable.model().writes.is_empty());
}

#[test]
fn smu_timeout_is_bounded_and_never_submits_when_busy() {
    let mailbox = SmnTest::new(Mailbox::default());
    let mut elapsed = 0;
    assert_eq!(
        smu::request(&mailbox.smn(), 1, [0; 6], &mut Delay(|us| elapsed += us)),
        Err(smu::Error::BusyTimeout)
    );
    assert_eq!(elapsed, 10_000_000);
    assert!(mailbox.model().writes.is_empty());
    mailbox.model().responses = [1, 0xfe].into();
    assert_eq!(
        smu::request(&mailbox.smn(), 1, [0; 6], &mut Delay(|_| {})),
        Err(smu::Error::Rejected(0xfe))
    );
}

#[test]
fn raven2_late_df_clears_bit11_after_setting_power_down_policy() {
    for raven2 in [false, true] {
        for old in [0, 0x80000020, 0x80000820] {
            let df = DfSpace::default();
            df.regs.borrow_mut().insert((5, 0x300, None), old);
            fabric::late_init(&df.df(), raven2);
            let expected =
                ((old & !(1 << 5)) | 0x3b00) & if raven2 { !(1 << 11) } else { u32::MAX };
            assert_eq!(df.regs.borrow()[&(5, 0x300, None)], expected);
            assert_eq!(df.writes.borrow()[0], ((5, 0x300, None), expected));
        }
    }
}

#[test]
fn fabric_route_validation_is_atomic_and_programming_is_idempotent() {
    let df = DfSpace::default();
    let die: super::Die<&DfSpace> = unsafe { crate::fam17h::topology::dies(&df) }
        .next()
        .unwrap();
    for i in 0..8 {
        df.regs.borrow_mut().insert((0, 0xc0 + i * 8, None), 3);
    }
    assert_eq!(
        fabric::ensure_legacy_routes(&die, 0xd0000000),
        Err(fabric::Error::NoIoWindow)
    );
    assert!(df.writes.borrow().is_empty());
    df.regs.borrow_mut().clear();
    fabric::ensure_legacy_routes(&die, 0xd0000000).unwrap();
    // MMIO pair 0: 0xd0000000-0xfebfffff, I/O pair 0: 0-0xffff, VGA: all
    // to the IOMS (fabric ID 9).
    let regs = df.regs.borrow().clone();
    assert_eq!(
        regs.into_iter().collect::<Vec<_>>(),
        vec![
            ((0, 0x80, None), 0x91),
            ((0, 0xc0, None), 3),
            ((0, 0xc4, None), 0xf009),
            ((0, 0x200, None), 0xd000),
            ((0, 0x204, None), 0xfebf),
            ((0, 0x208, None), 0x93),
        ]
    );
    // The control register is written last, the I/O base before its limit.
    assert_eq!(df.writes.borrow()[2], ((0, 0x208, None), 0x93));
    assert_eq!(df.writes.borrow()[3], ((0, 0xc0, None), 3));
    df.writes.borrow_mut().clear();
    fabric::ensure_legacy_routes(&die, 0xd0000000).unwrap();
    assert_eq!(*df.writes.borrow(), vec![((0, 0x80, None), 0x91)]);
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
