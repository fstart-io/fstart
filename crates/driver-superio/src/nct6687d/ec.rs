// SPDX-License-Identifier: MIT OR Apache-2.0
//! NCT6687D firmware-port EC transactions from nct6687d_ec.c.
//! The page/index/data endpoints implement
//! existing tock traits; real hardware uses fstart-core PioRegister.
//!
//! A borrowed register is a transaction guard. Tock `.modify()` consequently
//! performs read and write within ONE selected page/index transaction, exactly
//! like the source's and/or operations, then Drop writes the closing 0xff.
//! Caller owns the I/O window and serializes access against SMM/other agents.

use core::marker::PhantomData;
use fstart_core::pio::PioRegister;
use tock_registers::{
    RegisterLongName,
    interfaces::{Readable, Writeable},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidIoBase,
}

pub struct Ec<P, I, D> {
    page: P,
    index: I,
    data: D,
}
impl<P, I, D> Ec<P, I, D> {
    /// Construct from owned page/index/data registers, without accessing them.
    pub const fn new(page: P, index: I, data: D) -> Self {
        Self { page, index, data }
    }
}
impl Ec<PioRegister<u8>, PioRegister<u8>, PioRegister<u8>> {
    /// Source PNP_IO0 mask 0x0ff8. Only port0 (base+0/+1/+2) is used; port1
    /// belongs to software. PnP resource/decode setup and fan programming are
    /// separate operations, not implied by successful construction.
    pub fn ports(base: u16) -> Result<Self, Error> {
        if base == 0 || base & !0x0ff8 != 0 {
            return Err(Error::InvalidIoBase);
        }
        Ok(Self::new(
            PioRegister::new(base),
            PioRegister::new(base + 1),
            PioRegister::new(base + 2),
        ))
    }
}
impl<P: Writeable<T = u8>, I: Writeable<T = u8>, D> Ec<P, I, D> {
    pub fn register<R: RegisterLongName>(&mut self, address: u16) -> Register<'_, P, I, D, R> {
        self.select(address, false)
    }
    /// Source `*_page_ff`: additionally send 0xff to INDEX before the index.
    /// This is not an implicit request for page 0xff.
    pub fn register_ff<R: RegisterLongName>(&mut self, address: u16) -> Register<'_, P, I, D, R> {
        self.select(address, true)
    }
    fn select<R: RegisterLongName>(
        &mut self,
        address: u16,
        reset_index: bool,
    ) -> Register<'_, P, I, D, R> {
        let register = Register {
            ec: self,
            _register: PhantomData,
        };
        let [page, index] = address.to_be_bytes();
        register.ec.page.set(0xff);
        register.ec.page.set(page);
        if reset_index {
            register.ec.index.set(0xff);
        }
        register.ec.index.set(index);
        register
    }
}

#[must_use = "dropping the register closes its EC transaction"]
pub struct Register<'a, P: Writeable<T = u8>, I, D, R: RegisterLongName = ()> {
    ec: &'a mut Ec<P, I, D>,
    _register: PhantomData<R>,
}
impl<P: Writeable<T = u8>, I, D: Readable<T = u8>, R: RegisterLongName> Readable
    for Register<'_, P, I, D, R>
{
    type T = u8;
    type R = R;
    fn get(&self) -> u8 {
        self.ec.data.get()
    }
}
impl<P: Writeable<T = u8>, I, D: Writeable<T = u8>, R: RegisterLongName> Writeable
    for Register<'_, P, I, D, R>
{
    type T = u8;
    type R = R;
    fn set(&self, value: u8) {
        self.ec.data.set(value);
    }
}
impl<P: Writeable<T = u8>, I, D, R: RegisterLongName> Drop for Register<'_, P, I, D, R> {
    fn drop(&mut self) {
        self.ec.page.set(0xff);
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::{cell::RefCell, rc::Rc, vec::Vec};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Event {
        Write(u16, u8),
        Read(u16),
    }
    struct State {
        events: Vec<Event>,
        data: u8,
        fail_data_read: bool,
    }
    struct Port {
        address: u16,
        state: Rc<RefCell<State>>,
    }
    impl Readable for Port {
        type T = u8;
        type R = ();
        fn get(&self) -> u8 {
            let (value, fail) = {
                let mut state = self.state.borrow_mut();
                state.events.push(Event::Read(self.address));
                let fail = state.fail_data_read;
                state.fail_data_read = false;
                (state.data, fail)
            };
            assert!(!fail, "simulated endpoint failure");
            value
        }
    }
    impl Writeable for Port {
        type T = u8;
        type R = ();
        fn set(&self, value: u8) {
            let mut state = self.state.borrow_mut();
            state.events.push(Event::Write(self.address, value));
            if self.address == 0xa22 {
                state.data = value;
            }
        }
    }
    fn fake(data: u8) -> (Ec<Port, Port, Port>, Rc<RefCell<State>>) {
        let state = Rc::new(RefCell::new(State {
            events: Vec::new(),
            data,
            fail_data_read: false,
        }));
        (
            Ec::new(
                Port {
                    address: 0xa20,
                    state: state.clone(),
                },
                Port {
                    address: 0xa21,
                    state: state.clone(),
                },
                Port {
                    address: 0xa22,
                    state: state.clone(),
                },
            ),
            state,
        )
    }
    #[test]
    fn nct6687d_ec_guard_closes_page_on_drop_and_host_unwind() {
        let (mut ec, state) = fake(0);
        {
            let _register = ec.register::<()>(0x0110);
        }
        assert_eq!(
            state.borrow().events.last(),
            Some(&Event::Write(0xa20, 0xff))
        );
        state.borrow_mut().events.clear();
        state.borrow_mut().fail_data_read = true;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            ec.register::<()>(0x0255).get();
        }));
        assert!(result.is_err());
        assert_eq!(
            state.borrow().events.last(),
            Some(&Event::Write(0xa20, 0xff))
        );
        assert_eq!(ec.register::<()>(0x0001).get(), 0);
    }

    #[test]
    fn nct6687d_ec_uses_valid_firmware_port_zero_not_software_port_one() {
        for base in [0, 0xa21, 0x1a20, 0xfff8] {
            assert!(matches!(Ec::ports(base), Err(Error::InvalidIoBase)));
        }
        let ec = Ec::ports(0xa20).unwrap();
        assert_eq!(
            (ec.page.port(), ec.index.port(), ec.data.port()),
            (0xa20, 0xa21, 0xa22)
        );
    }
}
