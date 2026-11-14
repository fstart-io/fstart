// SPDX-License-Identifier: MIT OR Apache-2.0
//! A byte-addressed register model with a write log, and an FCH built
//! from models.
use fstart_core::services::ManualTimer;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use super::Fch;
use crate::smn::model::Model;
use crate::smu::{ARGUMENT_COUNT, Error as SmuError, SmuMessages};
use crate::window::Window;

#[derive(Default)]
pub struct State {
    pub bytes: BTreeMap<u32, u8>,
    /// `(offset, width, value)` of every write.
    pub log: Vec<(u32, u8, u32)>,
    /// Values returned (once each, then sticky) by reads of an offset.
    pub reads: BTreeMap<u32, Vec<u32>>,
    /// Index/data pairs `data port -> index port`; the data port then
    /// reads and writes `indexed[(data, index)]`.
    pub pairs: BTreeMap<u32, u32>,
    pub indexed: BTreeMap<(u32, u32), u32>,
}

/// A memory-like register space; unwritten bytes read 0.
#[derive(Clone, Default)]
pub struct Mem(pub Rc<RefCell<State>>);

impl Mem {
    /// I/O ports with the FCH index/data pairs (PM, AB, PCI_INTR, RTC).
    pub fn ports() -> Self {
        let m = Self::default();
        m.0.borrow_mut().pairs =
            [(0xcd7, 0xcd6), (0xcdc, 0xcd8), (0xc01, 0xc00), (0x71, 0x70)].into();
        m
    }
    /// The value behind `index` of the pair with data port `data`.
    pub fn at(&self, data: u32, index: u32) -> u32 {
        self.0
            .borrow()
            .indexed
            .get(&(data, index))
            .copied()
            .unwrap_or(0)
    }
    pub fn set_at(&self, data: u32, index: u32, value: u32) {
        self.0.borrow_mut().indexed.insert((data, index), value);
    }
    fn pair(&self, off: u32) -> Option<u32> {
        let index = *self.0.borrow().pairs.get(&off)?;
        let width = if index == 0xcd8 { 4 } else { 1 };
        Some(self.get(index, width))
    }
    pub fn get(&self, off: u32, width: u8) -> u32 {
        let s = self.0.borrow();
        (0..u32::from(width)).fold(0, |v, i| {
            v | u32::from(s.bytes.get(&(off + i)).copied().unwrap_or(0)) << (8 * i)
        })
    }
    pub fn put(&self, off: u32, width: u8, value: u32) {
        let mut s = self.0.borrow_mut();
        (0..u32::from(width)).for_each(|i| {
            s.bytes.insert(off + i, (value >> (8 * i)) as u8);
        });
    }
    pub fn writes(&self) -> Vec<(u32, u8, u32)> {
        self.0.borrow().log.clone()
    }
    pub fn writes_to(&self, off: u32) -> Vec<u32> {
        self.writes()
            .into_iter()
            .filter(|w| w.0 == off)
            .map(|w| w.2)
            .collect()
    }
    fn read(&self, off: u32, width: u8) -> u32 {
        let scripted = {
            let mut s = self.0.borrow_mut();
            s.reads.get_mut(&off).and_then(|v| {
                if v.len() > 1 {
                    Some(v.remove(0))
                } else {
                    v.first().copied()
                }
            })
        };
        if let Some(index) = self.pair(off) {
            return scripted.unwrap_or_else(|| self.at(off, index));
        }
        scripted.unwrap_or_else(|| self.get(off, width))
    }
    fn write(&self, off: u32, width: u8, value: u32) {
        match self.pair(off) {
            Some(index) => self.set_at(off, index, value),
            None => self.put(off, width, value),
        }
        self.0.borrow_mut().log.push((off, width, value));
    }
}

impl Window for Mem {
    fn read32(&self, off: u32) -> u32 {
        self.read(off, 4)
    }
    fn write32(&self, off: u32, value: u32) {
        self.write(off, 4, value)
    }
    fn read16(&self, off: u32) -> u16 {
        self.read(off, 2) as u16
    }
    fn write16(&self, off: u32, value: u16) {
        self.write(off, 2, value.into())
    }
    fn read8(&self, off: u32) -> u8 {
        self.read(off, 1) as u8
    }
    fn write8(&self, off: u32, value: u8) {
        self.write(off, 1, value.into())
    }
}

/// An SMU that answers OK and records `(id, args)`.
#[derive(Clone, Default)]
pub struct Smu(pub Rc<RefCell<Vec<Message>>>);
pub type Message = (u32, [u32; ARGUMENT_COUNT]);
impl SmuMessages for Smu {
    fn send(&self, id: u32, args: &mut [u32; ARGUMENT_COUNT]) -> Result<(), SmuError> {
        self.0.borrow_mut().push((id, *args));
        Ok(())
    }
}
impl Smu {
    pub fn ids(&self) -> Vec<u32> {
        self.0.borrow().iter().map(|m| m.0).collect()
    }
}

pub type TestFch = Fch<Mem, Mem, Mem, Model, ManualTimer>;

pub fn fch() -> TestFch {
    let pci = Model::default();
    // The LPC bridge and SMBus function read 0 until written.
    (0..0x100).step_by(4).for_each(|off| {
        pci.0.borrow_mut().config.insert((0, 0x14, 3, off), 0);
        pci.0.borrow_mut().config.insert((0, 0x14, 0, off), 0);
    });
    Fch {
        acpi: Mem::default(),
        mmio: Mem::default(),
        io: Mem::ports(),
        pci,
        bus: 0,
        timer: ManualTimer::new(),
    }
}

/// SMN dword at `addr` on `bus`.
pub fn smn(pci: &Model, bus: u8, addr: u32) -> u32 {
    pci.0.borrow().smn.get(&(bus, addr)).copied().unwrap_or(0)
}

/// SMN writes on `bus` in order: `(addr, value)`.
pub fn smn_writes(pci: &Model, bus: u8) -> Vec<(u32, u32)> {
    pci.0
        .borrow()
        .log
        .iter()
        .filter(|w| w.0 == bus)
        .map(|w| (w.1, w.2))
        .collect()
}
