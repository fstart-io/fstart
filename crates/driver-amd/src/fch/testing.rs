// SPDX-License-Identifier: MIT OR Apache-2.0
//! A host model of the FCH register spaces for tests: little-endian bytes
//! per space, the I/O index/data pairs, write-one-to-clear registers, and
//! a log of every access. [`Fch`] is the [`FchHw`] of a model whose
//! spaces log in one order, so a test sees the accesses of all windows
//! as the hardware would.
extern crate std;
use super::hw::{FchHw, acpimmio};
use crate::window::Window;
use core::cell::RefCell;
use fstart_core::services::{ManualTimer, Timer};
use fstart_pci::{ConfigRegionAccess, PciAddress};
use std::{
    collections::{BTreeMap, BTreeSet},
    vec::Vec,
};

/// The register spaces of an FCH model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Space {
    /// Physical MMIO below 4 GiB: the ACPI MMIO, SPI, A-Link-to-AHB.
    Mmio,
    /// x86 I/O ports.
    Io,
    /// Config space of the LPC bridge D14F3.
    Lpc,
    /// Config space of the SMBus function D14F0.
    Smbus,
}

/// A model of the FCH register spaces, with config space for its SMN.
pub trait Spaces: ConfigRegionAccess {
    fn read(&self, space: Space, address: u32, bytes: u8) -> u32;
    fn write(&self, space: Space, address: u32, bytes: u8, value: u32);
}

/// One space of a model from `base` as a window, with exact-width accesses.
pub struct View<'a, S> {
    model: &'a S,
    space: Space,
    base: u32,
}
impl<S> Clone for View<'_, S> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<S> Copy for View<'_, S> {}
impl<S: Spaces> Window for View<'_, S> {
    fn read32(&self, off: u32) -> u32 {
        Spaces::read(self.model, self.space, self.base + off, 4)
    }
    fn write32(&self, off: u32, value: u32) {
        Spaces::write(self.model, self.space, self.base + off, 4, value)
    }
    fn read16(&self, off: u32) -> u16 {
        Spaces::read(self.model, self.space, self.base + off, 2) as u16
    }
    fn write16(&self, off: u32, value: u16) {
        Spaces::write(self.model, self.space, self.base + off, 2, value.into())
    }
    fn read8(&self, off: u32) -> u8 {
        Spaces::read(self.model, self.space, self.base + off, 1) as u8
    }
    fn write8(&self, off: u32, value: u8) {
        Spaces::write(self.model, self.space, self.base + off, 1, value.into())
    }
}

/// FCH 0 of a model, as [`super::hw::Native`] reaches it.
pub struct Fch<'a, S, T> {
    acpi: View<'a, S>,
    mmio: View<'a, S>,
    io: View<'a, S>,
    model: &'a S,
    timer: T,
}
impl<'a, S, T> Fch<'a, S, T> {
    pub fn new(model: &'a S, timer: T) -> Self {
        let view = |space, base| View { model, space, base };
        Self {
            acpi: view(Space::Mmio, acpimmio::BASE as u32),
            mmio: view(Space::Mmio, 0),
            io: view(Space::Io, 0),
            model,
            timer,
        }
    }
}
impl<'a, S: Spaces, T: Timer> FchHw for Fch<'a, S, T> {
    type Acpi = View<'a, S>;
    type Mmio = View<'a, S>;
    type Io = View<'a, S>;
    type Config = View<'a, S>;
    type Pci = &'a S;
    type Timer = T;

    fn acpi(&self) -> &Self::Acpi {
        &self.acpi
    }
    fn mmio(&self) -> &Self::Mmio {
        &self.mmio
    }
    fn io(&self) -> &Self::Io {
        &self.io
    }
    fn config(&self, function: PciAddress) -> Self::Config {
        let space = match (function.bus(), function.device(), function.function()) {
            (0, 0x14, 3) => Space::Lpc,
            (0, 0x14, 0) => Space::Smbus,
            _ => panic!("no model of {function:?}"),
        };
        View {
            model: self.model,
            space,
            base: 0,
        }
    }
    fn pci(&self) -> &&'a S {
        &self.model
    }
    fn bus(&self) -> u8 {
        0
    }
    fn timer(&self) -> &T {
        &self.timer
    }
}

/// The I/O index/data pairs: (index port, data port).
const PAIRS: [(u16, u16); 4] = [(0xc00, 0xc01), (0x70, 0x71), (0xcd6, 0xcd7), (0xcd8, 0xcdc)];
fn pair(space: Space, address: u32) -> Option<(u16, bool)> {
    if space != Space::Io {
        return None;
    }
    PAIRS
        .iter()
        .find_map(|&(index, data)| match address as u16 {
            p if p == index => Some((index, true)),
            p if p == data => Some((index, false)),
            _ => None,
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read(Space, u32),
    Write(Space, u32, u8, u32),
}

#[derive(Default)]
pub struct State {
    pub bytes: BTreeMap<(Space, u32), u8>,
    /// Index/data pairs: (index port, index) -> value.
    pub indexed: BTreeMap<(u16, u32), u32>,
    selected: BTreeMap<u16, u32>,
    /// Write-one-to-clear registers (their first byte).
    pub w1c: BTreeSet<(Space, u32)>,
    pub log: Vec<Access>,
}

#[derive(Default)]
pub struct World(pub RefCell<State>);
impl World {
    pub fn get(&self, space: Space, address: u32, bytes: u8) -> u32 {
        let state = self.0.borrow();
        (0..bytes).rev().fold(0, |value, i| {
            let byte = state.bytes.get(&(space, address + u32::from(i))).copied();
            (value << 8) | u32::from(byte.unwrap_or(0))
        })
    }
    pub fn set(&self, space: Space, address: u32, bytes: u8, value: u32) {
        let mut state = self.0.borrow_mut();
        for i in 0..bytes {
            state
                .bytes
                .insert((space, address + u32::from(i)), (value >> (8 * i)) as u8);
        }
    }
    pub fn indexed(&self, index_port: u16, index: u32) -> u32 {
        let state = self.0.borrow();
        state
            .indexed
            .get(&(index_port, index))
            .copied()
            .unwrap_or(0)
    }
    pub fn set_indexed(&self, index_port: u16, index: u32, value: u32) {
        self.0
            .borrow_mut()
            .indexed
            .insert((index_port, index), value);
    }
    /// The writes, in order.
    pub fn writes(&self) -> Vec<(Space, u32, u8, u32)> {
        self.0
            .borrow()
            .log
            .iter()
            .filter_map(|access| match *access {
                Access::Write(space, address, bytes, value) => Some((space, address, bytes, value)),
                Access::Read(..) => None,
            })
            .collect()
    }
    pub fn reads(&self) -> Vec<(Space, u32)> {
        self.0
            .borrow()
            .log
            .iter()
            .filter_map(|access| match *access {
                Access::Read(space, address) => Some((space, address)),
                Access::Write(..) => None,
            })
            .collect()
    }
    pub fn clear_log(&self) {
        self.0.borrow_mut().log.clear();
    }
}
impl World {
    /// The model's FCH.
    pub fn fch(&self) -> Fch<'_, Self, ManualTimer> {
        Fch::new(self, ManualTimer::new())
    }
}
/// The model has no config space but the FCH's.
impl ConfigRegionAccess for World {
    unsafe fn read(&self, address: PciAddress, _: u16) -> u32 {
        panic!("no model of {address:?}")
    }
    unsafe fn write(&self, address: PciAddress, _: u16, _: u32) {
        panic!("no model of {address:?}")
    }
}
impl Spaces for World {
    fn read(&self, space: Space, address: u32, bytes: u8) -> u32 {
        self.0.borrow_mut().log.push(Access::Read(space, address));
        if let Some((index, false)) = pair(space, address) {
            let state = self.0.borrow();
            let selected = state.selected.get(&index).copied().unwrap_or(0);
            return state.indexed.get(&(index, selected)).copied().unwrap_or(0);
        }
        self.get(space, address, bytes)
    }
    fn write(&self, space: Space, address: u32, bytes: u8, value: u32) {
        let mut state = self.0.borrow_mut();
        state.log.push(Access::Write(space, address, bytes, value));
        match pair(space, address) {
            Some((index, true)) => {
                state.selected.insert(index, value);
                return;
            }
            Some((index, false)) => {
                let selected = state.selected.get(&index).copied().unwrap_or(0);
                state.indexed.insert((index, selected), value);
                return;
            }
            None => {}
        }
        let w1c = state.w1c.contains(&(space, address));
        drop(state);
        let value = if w1c {
            self.get(space, address, bytes) & !value
        } else {
            value
        };
        self.set(space, address, bytes, value);
    }
}
