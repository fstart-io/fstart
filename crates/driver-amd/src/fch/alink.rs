// SPDX-License-Identifier: MIT OR Apache-2.0
//! A-Link (AB) configuration through the index/data pair at 0xcd8/0xcdc.
//! The PMxE0 decode of the pair is the caller's.
use crate::table::{self, Record};
use crate::window::{At, Reg, Window};
use tock_registers::{
    interfaces::{Readable, Writeable},
    register_bitfields,
};

/// The index/data pair.
pub const INDEX: u16 = 0xcd8;
pub const DATA: u16 = 0xcdc;

register_bitfields![u32,
    /// A-Link index: register space in [31:29].
    AbIndex [
        ADDRESS OFFSET(0) NUMBITS(29) [],
        SPACE OFFSET(29) NUMBITS(3) [Abcfg = 6]
    ]
];

/// A record of an ABCFG table ([`crate::table`]); the tables are
/// vendor values.
pub type Rmw = Record<u32, u32>;

/// ABCFG register `register`. Every access selects it and clears the index
/// afterwards (RPR 4.2: required for the 0x100NN registers).
pub struct Abcfg<'a, W> {
    io: &'a W,
    register: u32,
}
/// ABCFG register `register` through the I/O window `io`.
pub fn abcfg<W: Window>(io: &W, register: u32) -> Abcfg<'_, W> {
    Abcfg { io, register }
}
impl<W: Window> Abcfg<'_, W> {
    fn access<T>(&self, f: impl FnOnce(&Reg<&W, u32>) -> T) -> T {
        let index = self.io.at(At::<u32, AbIndex::Register>::new(INDEX.into()));
        index.write(AbIndex::SPACE::Abcfg + AbIndex::ADDRESS.val(self.register));
        let value = f(&self.io.at(At::new(DATA.into())));
        index.set(0);
        value
    }
}
impl<W: Window> Readable for Abcfg<'_, W> {
    type T = u32;
    type R = ();
    fn get(&self) -> u32 {
        self.access(|data| data.get())
    }
}
impl<W: Window> Writeable for Abcfg<'_, W> {
    type T = u32;
    type R = ();
    fn set(&self, value: u32) {
        self.access(|data| data.set(value))
    }
}

/// Apply an ABCFG table in order.
pub fn apply(io: &impl Window, records: &[Rmw]) {
    table::apply(records.iter().copied(), |register| abcfg(io, register));
}
