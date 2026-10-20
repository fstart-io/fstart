// SPDX-License-Identifier: MIT OR Apache-2.0
//! A-Link (AB) configuration through the index/data pair at 0xcd8/0xcdc.
//! The PMxE0 decode of the pair is the caller's.
use super::{Bus, Reg};
use crate::fam17h::table::{self, Record};
use tock_registers::{
    interfaces::{Readable, Writeable},
    register_bitfields,
};

/// The index/data pair.
pub const INDEX: u16 = 0xcd8;
pub const DATA: u16 = 0xcdc;

register_bitfields![u32,
    /// A-Link index: register space in [31:29].
    AbIndex [ADDRESS OFFSET(0) NUMBITS(29) [], SPACE OFFSET(29) NUMBITS(3) [Abcfg = 6]]
];

/// A record of an ABCFG table ([`crate::fam17h::table`]); the tables are
/// vendor values.
pub type Rmw = Record<u32, u32>;

/// ABCFG register `register`. Every access selects it and clears the index
/// afterwards (RPR 4.2: required for the 0x100NN registers).
pub struct Abcfg<'a, B> {
    bus: &'a B,
    register: u32,
}
pub fn abcfg<B: Bus>(bus: &B, register: u32) -> Abcfg<'_, B> {
    Abcfg { bus, register }
}
impl<B: Bus> Abcfg<'_, B> {
    fn access<T>(&self, f: impl FnOnce(&super::BusRegister<'_, B, u32, ()>) -> T) -> T {
        let index = self.bus.reg(Reg::<u32, AbIndex::Register>::io(INDEX));
        index.write(AbIndex::SPACE::Abcfg + AbIndex::ADDRESS.val(self.register));
        let value = f(&self.bus.reg(Reg::<u32>::io(DATA)));
        index.set(0);
        value
    }
}
impl<B: Bus> Readable for Abcfg<'_, B> {
    type T = u32;
    type R = ();
    fn get(&self) -> u32 {
        self.access(|data| data.get())
    }
}
impl<B: Bus> Writeable for Abcfg<'_, B> {
    type T = u32;
    type R = ();
    fn set(&self, value: u32) {
        self.access(|data| data.set(value))
    }
}

/// Apply an ABCFG table in order.
pub fn apply(bus: &impl Bus, records: &[Rmw]) {
    table::apply(records.iter().copied(), |register| abcfg(bus, register));
}
