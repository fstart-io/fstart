// SPDX-License-Identifier: MIT OR Apache-2.0
//! A host model of the FCH register spaces for tests: little-endian bytes
//! per space, the I/O index/data pairs, write-one-to-clear registers, and
//! a log of every access.
extern crate std;
use super::{Bus, Space};
use core::cell::RefCell;
use std::{
    collections::{BTreeMap, BTreeSet},
    vec::Vec,
};

/// The I/O index/data pairs: (index port, data port).
const PAIRS: [(u16, u16); 4] = [(0xc00, 0xc01), (0x70, 0x71), (0xcd6, 0xcd7), (0xcd8, 0xcdc)];
fn pair(space: Space, address: u32) -> Option<(u16, bool)> {
    if space != Space::Io {
        return None;
    }
    PAIRS.iter().find_map(|&(index, data)| match address as u16 {
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
        self.0.borrow_mut().indexed.insert((index_port, index), value);
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
impl Bus for World {
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
