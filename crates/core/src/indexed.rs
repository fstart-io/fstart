//! Registers behind an index/data pair.
//!
//! Many chipset register spaces are reached by writing a register number to
//! an index register and then accessing a data register: Super I/O and CMOS
//! ports, the AMD SMN and Data Fabric (FICAA) windows in PCI config space,
//! the IOAPIC select/window pair. [`Indexed`] is one such register over any
//! index and data register that implement the tock interfaces, so the
//! register's own bitfields work with `read`, `write`, `modify`, etc.
//!
//! ```ignore
//! let ldn = Indexed::<_, _, LDN::Register>::new(index_port, data_port, 0x07);
//! ldn.modify(LDN::NUMBER.val(3));
//! ```
//!
//! The caller serializes the pair: every access selects its register first,
//! and nothing else may change the selection in between.

use core::marker::PhantomData;
use tock_registers::RegisterLongName;
use tock_registers::interfaces::{Readable, Writeable};

/// Register `at` of the space behind the `index` and `data` registers, with
/// the bitfields `R`.
pub struct Indexed<I: Writeable, D, R: RegisterLongName = ()> {
    index: I,
    data: D,
    at: I::T,
    _reg: PhantomData<R>,
}

impl<I: Writeable + Clone, D: Clone, R: RegisterLongName> Clone for Indexed<I, D, R> {
    fn clone(&self) -> Self {
        Self::new(self.index.clone(), self.data.clone(), self.at)
    }
}
impl<I: Writeable + Copy, D: Copy, R: RegisterLongName> Copy for Indexed<I, D, R> {}

impl<I: Writeable, D, R: RegisterLongName> Indexed<I, D, R> {
    pub const fn new(index: I, data: D, at: I::T) -> Self {
        Self {
            index,
            data,
            at,
            _reg: PhantomData,
        }
    }
}

impl<I: Writeable, D: Readable, R: RegisterLongName> Readable for Indexed<I, D, R> {
    type T = D::T;
    type R = R;

    #[inline(always)]
    fn get(&self) -> D::T {
        self.index.set(self.at);
        self.data.get()
    }
}

impl<I: Writeable, D: Writeable, R: RegisterLongName> Writeable for Indexed<I, D, R> {
    type T = D::T;
    type R = R;

    #[inline(always)]
    fn set(&self, value: D::T) {
        self.index.set(self.at);
        self.data.set(value);
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use core::cell::RefCell;
    use std::vec::Vec;
    use tock_registers::interfaces::ReadWriteable;
    use tock_registers::register_bitfields;

    register_bitfields![u8, Config [ENABLE OFFSET(0) NUMBITS(1) [], MODE OFFSET(4) NUMBITS(2) []]];

    /// An index/data pair over 32 bytes, logging every port access.
    #[derive(Default)]
    struct Space {
        selected: u8,
        bytes: [u8; 32],
        log: Vec<(bool, u8)>,
    }
    #[derive(Clone, Copy)]
    struct Port<'a>(&'a RefCell<Space>, bool);
    impl Readable for Port<'_> {
        type T = u8;
        type R = ();
        fn get(&self) -> u8 {
            let space = self.0.borrow();
            space.bytes[usize::from(space.selected)]
        }
    }
    impl Writeable for Port<'_> {
        type T = u8;
        type R = ();
        fn set(&self, value: u8) {
            let mut space = self.0.borrow_mut();
            space.log.push((self.1, value));
            if self.1 {
                space.selected = value;
            } else {
                let selected = space.selected;
                space.bytes[usize::from(selected)] = value;
            }
        }
    }

    #[test]
    fn every_access_selects_the_register_first() {
        let space = RefCell::new(Space::default());
        space.borrow_mut().bytes[0x13] = 0xc0;
        let config = Indexed::<_, _, Config::Register>::new(Port(&space, true), Port(&space, false), 0x13);
        config.modify(Config::ENABLE::SET + Config::MODE.val(2));
        let space = space.into_inner();
        assert_eq!(space.bytes[0x13], 0xe1);
        assert_eq!(space.log, [(true, 0x13), (true, 0x13), (false, 0xe1)]);
    }
}
