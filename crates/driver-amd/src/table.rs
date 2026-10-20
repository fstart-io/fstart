// SPDX-License-Identifier: MIT OR Apache-2.0
//! Vendor register tables: sequences whose bits mostly have no known
//! meaning stay raw records, applied in their original order (repeated
//! writes are part of the sequence). One interpreter for every register
//! space: a record names its register with an address `A` the transport
//! resolves to a tock register (SMN, Data Fabric, MSR, FCH, A-Link).
//!
//! ```ignore
//! const TABLE: &[Record<SmnReg, u32>] = &[rmw(SmnReg::new(0x13b1_0004), 0xffff, 0x10)];
//! table::apply(TABLE.iter().copied(), |reg| smn.reg(reg));
//! ```
use tock_registers::{
    UIntLike,
    interfaces::{Readable, Writeable},
};

/// What a record does to its register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op<A, T> {
    /// `reg = (reg & !clear) | set`
    Update { clear: T, set: T },
    /// `reg = value`, without reading it.
    Write(T),
    /// `reg = from`
    Copy { from: A },
}

/// One record of a vendor table, applied if its condition `when` holds
/// ([`apply_when`]); `()` for unconditional records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Record<A, T, C = ()> {
    pub reg: A,
    pub op: Op<A, T>,
    pub when: C,
}
pub const fn rmw<A, T>(reg: A, clear: T, set: T) -> Record<A, T> {
    Record {
        reg,
        op: Op::Update { clear, set },
        when: (),
    }
}
pub const fn write<A, T>(reg: A, value: T) -> Record<A, T> {
    Record {
        reg,
        op: Op::Write(value),
        when: (),
    }
}
pub const fn copy<A, T>(reg: A, from: A) -> Record<A, T> {
    Record {
        reg,
        op: Op::Copy { from },
        when: (),
    }
}
impl<A: Copy, T: Copy> Record<A, T> {
    /// The same record, applied only where `condition` holds.
    pub const fn when<C>(self, condition: C) -> Record<A, T, C> {
        Record {
            reg: self.reg,
            op: self.op,
            when: condition,
        }
    }
}
impl<A, T, C> Record<A, T, C> {
    /// The record without its condition.
    pub fn unconditional(self) -> Record<A, T> {
        Record {
            reg: self.reg,
            op: self.op,
            when: (),
        }
    }
}

/// Apply `table` in order, `reg` resolving a record's register.
pub fn apply<A, T: UIntLike, R: Readable<T = T> + Writeable<T = T>>(
    table: impl IntoIterator<Item = Record<A, T>>,
    reg: impl Fn(A) -> R,
) {
    apply_when(table, |_| true, reg)
}

/// Apply the records of `table` whose condition `holds`, in order.
pub fn apply_when<A, T: UIntLike, C, R: Readable<T = T> + Writeable<T = T>>(
    table: impl IntoIterator<Item = Record<A, T, C>>,
    holds: impl Fn(&C) -> bool,
    reg: impl Fn(A) -> R,
) {
    for record in table.into_iter().filter(|record| holds(&record.when)) {
        let target = reg(record.reg);
        match record.op {
            Op::Update { clear, set } => target.set((target.get() & !clear) | set),
            Op::Write(value) => target.set(value),
            Op::Copy { from } => target.set(reg(from).get()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tock_registers::registers::InMemoryRegister;

    #[test]
    fn copy_write_and_update_in_order_where_the_condition_holds() {
        struct Reg<'a>(&'a InMemoryRegister<u8>);
        impl Readable for Reg<'_> {
            type T = u8;
            type R = ();
            fn get(&self) -> u8 {
                self.0.get()
            }
        }
        impl Writeable for Reg<'_> {
            type T = u8;
            type R = ();
            fn set(&self, value: u8) {
                self.0.set(value)
            }
        }
        let regs: [InMemoryRegister<u8>; 3] = [0xf0, 0x00, 0x12].map(InMemoryRegister::new);
        apply_when(
            [
                rmw(0, 0x3f, 0x01).when(true),
                copy(1, 0).when(true),
                write(0, 0x05).when(true),
                write(2, 0xff).when(false),
            ],
            |&when| when,
            |i: usize| Reg(&regs[i]),
        );
        assert_eq!(regs.map(|r| r.get()), [0x05, 0xc1, 0x12]);
    }
}
