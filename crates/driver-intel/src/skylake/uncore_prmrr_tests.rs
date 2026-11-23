// SPDX-License-Identifier: MIT OR Apache-2.0
extern crate std;
use super::*;
use core::cell::{Cell, RefCell};
use std::vec::Vec;

pub(crate) struct Cpu {
    pub cap: Cell<u64>,
    pub supported: Cell<u64>,
    pub base: Cell<u64>,
    pub mask: Cell<u64>,
    pub drop_write: Cell<Option<u32>>,
    pub writes: RefCell<Vec<(u32, u64)>>,
}
impl Cpu {
    pub fn new() -> Self {
        Self {
            cap: Cell::new(1 << 12),
            supported: Cell::new(0x1e1),
            base: Cell::new(0),
            mask: Cell::new(0),
            drop_write: Cell::new(None),
            writes: RefCell::new(Vec::new()),
        }
    }
}
impl MsrAccess for Cpu {
    fn read(&self, index: u32) -> u64 {
        match index {
            0xfe => self.cap.get(),
            0x1fb => self.supported.get(),
            0x2f4 => self.base.get(),
            0x2f5 => self.mask.get(),
            _ => panic!("unexpected MSR {index:x}"),
        }
    }
    fn write(&self, index: u32, value: u64) {
        self.writes.borrow_mut().push((index, value));
        if self.drop_write.get() == Some(index) {
            return;
        }
        assert_eq!(self.mask.get() & (1 << 10), 0, "write under package lock");
        match index {
            0x2f4 => self.base.set(value),
            0x2f5 => self.mask.set(value),
            _ => panic!("unexpected write"),
        }
    }
}
pub(crate) fn request(size: u16) -> Request {
    Request::read_width(
        &Cpu::new(),
        Policy {
            features_enabled: size != 0,
            maximum_mib: size,
        },
        39,
    )
    .unwrap()
}
#[test]
fn native_uncore_prmrr_capability_config_and_source_size_selection() {
    let cpu = Cpu::new();
    for (max, expected) in [
        (0, 0),
        (1, 1),
        (31, 1),
        (32, 32),
        (63, 32),
        (64, 64),
        (128, 128),
        (256, 256),
    ] {
        let r = Request::read_width(
            &cpu,
            Policy {
                features_enabled: true,
                maximum_mib: max,
            },
            39,
        )
        .unwrap();
        assert_eq!(r.size_mib(), expected);
    }
    cpu.supported.set(2);
    assert_eq!(
        Request::read_width(
            &cpu,
            Policy {
                features_enabled: true,
                maximum_mib: 32
            },
            39
        ),
        Err(Error::InvalidSize)
    );
    cpu.cap.set(0);
    assert_eq!(
        Request::read_width(
            &cpu,
            Policy {
                features_enabled: true,
                maximum_mib: 32
            },
            39
        ),
        Err(Error::Unsupported)
    );
    let r = Request::read_width(
        &cpu,
        Policy {
            features_enabled: false,
            maximum_mib: 0,
        },
        39,
    )
    .unwrap();
    assert_eq!(
        lock(&cpu, r.placement(0).unwrap()),
        Ok(LockStatus::Unsupported)
    );
    assert!(cpu.writes.borrow().is_empty());
    assert_eq!(
        Request::read_width(
            &cpu,
            Policy {
                features_enabled: false,
                maximum_mib: 0
            },
            0
        ),
        Err(Error::InvalidAddress)
    );
}
#[test]
fn native_uncore_prmrr_real_base_mask_lock_and_idempotence() {
    for size in [1, 32, 64, 128, 256] {
        let cpu = Cpu::new();
        let p = request(size).placement(1024).unwrap();
        assert_eq!(lock(&cpu, p), Ok(LockStatus::Locked));
        let mask = (((1u64 << 39) - 1) & !((u64::from(size) << 20) - 1)) | 0xc00;
        assert_eq!(&*cpu.writes.borrow(), &[(0x2f4, 1 << 30), (0x2f5, mask)]);
        assert_eq!(lock(&cpu, p), Ok(LockStatus::Locked));
        assert_eq!(cpu.writes.borrow().len(), 2);
        cpu.base.set(0);
        assert_eq!(lock(&cpu, p), Err(Error::LockedMismatch));
        assert_eq!(cpu.writes.borrow().len(), 2);
    }
}
#[test]
fn native_uncore_prmrr_zero_size_still_locks_and_rejects_stale_base() {
    let cpu = Cpu::new();
    let p = request(0).placement(1000).unwrap();
    cpu.base.set(0x100000);
    assert_eq!(lock(&cpu, p), Err(Error::Conflict));
    assert!(cpu.writes.borrow().is_empty());
    cpu.base.set(0);
    assert_eq!(lock(&cpu, p), Ok(LockStatus::Locked));
    assert_eq!(&*cpu.writes.borrow(), &[(0x2f5, 0x400)]);
    assert_eq!(lock(&cpu, p), Ok(LockStatus::Locked));
    cpu.mask.set(0xc00);
    assert_eq!(lock(&cpu, p), Err(Error::LockedMismatch));
}
#[test]
fn native_uncore_prmrr_geometry_conflicts_and_fatal_partial_readback() {
    assert_eq!(request(64).placement(1056), Err(Error::InvalidAddress));
    let cpu = Cpu::new();
    let p = request(32).placement(1024).unwrap();
    cpu.mask.set(0x100);
    assert_eq!(lock(&cpu, p), Err(Error::Conflict));
    assert!(cpu.writes.borrow().is_empty());
    cpu.mask.set(0);
    cpu.drop_write.set(Some(0x2f5));
    assert_eq!(lock(&cpu, p), Err(Error::Readback));
    assert_eq!(cpu.base.get(), 1 << 30);
    assert_eq!(cpu.mask.get(), 0);
    assert_eq!(cpu.writes.borrow().len(), 2); // no fictitious rollback
    let cpu = Cpu::new();
    cpu.drop_write.set(Some(0x2f4));
    assert_eq!(lock(&cpu, p), Err(Error::Readback));
    assert_eq!(cpu.writes.borrow().len(), 1);
}

#[test]
fn native_uncore_prmrr_unavailable_capability_and_changed_supported_size_fail_closed() {
    let cpu = Cpu::new();
    cpu.cap.set(u64::MAX);
    assert_eq!(
        Request::read_width(
            &cpu,
            Policy {
                features_enabled: false,
                maximum_mib: 0
            },
            39
        ),
        Err(Error::Unsupported)
    );
    let p = request(32).placement(1024).unwrap();
    assert_eq!(lock(&cpu, p), Err(Error::Conflict));
    cpu.cap.set(1 << 12);
    cpu.supported.set(u64::MAX);
    assert_eq!(lock(&cpu, p), Err(Error::Conflict));
    cpu.supported.set(1);
    assert_eq!(lock(&cpu, p), Err(Error::Conflict));
    assert!(cpu.writes.borrow().is_empty());
    let r = Request::read_width(
        &cpu,
        Policy {
            features_enabled: true,
            maximum_mib: 1,
        },
        32,
    )
    .unwrap();
    assert_eq!(r.placement(4096), Err(Error::InvalidAddress));
}
