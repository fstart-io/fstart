// SPDX-License-Identifier: MIT OR Apache-2.0
//! A-Link (AB) configuration through the index/data pair at 0xcd8/0xcdc.
//! The PMxE0 decode of the pair is the caller's.
use super::{Bus, Reg};
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

/// One read-clear-set of an ABCFG register: (register, clear, set). The
/// tables are vendor values.
pub type Rmw = (u32, u32, u32);

/// ABCFG read-modify-write with the index cleared between the halves
/// (RPR 4.2: required for the 0x100NN registers).
pub fn abcfg(bus: &impl Bus, (register, clear, set): Rmw) {
    let index = bus.reg(Reg::<u32, AbIndex::Register>::io(INDEX));
    let data = bus.reg(Reg::<u32>::io(DATA));
    let select = AbIndex::SPACE::Abcfg + AbIndex::ADDRESS.val(register);
    index.write(select);
    let value = data.get();
    index.set(0);
    index.write(select);
    data.set((value & !clear) | set);
    index.set(0);
}

/// Apply an ABCFG table in order.
pub fn apply(bus: &impl Bus, table: &[Rmw]) {
    table.iter().for_each(|&rmw| abcfg(bus, rmw));
}
