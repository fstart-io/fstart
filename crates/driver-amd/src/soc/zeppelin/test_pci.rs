// SPDX-License-Identifier: MIT OR Apache-2.0
//! Host model of a multi-die Zeppelin for tests: the DF bus/node registers,
//! per-die SMN through D0F0 B8/BC of the die's root bus, and an SMU that
//! completes every message.
extern crate std;
use super::Die;
use crate::df::dies;
use crate::smu::v9::MAILBOX;
use fstart_pci::{ConfigRegionAccess, PciAddress};
use std::{cell::RefCell, collections::BTreeMap, vec::Vec};

/// A message a die's SMU received.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Message {
    pub die: u8,
    pub message: u32,
    pub arguments: [u32; 6],
}

pub struct Pci {
    buses: Vec<u8>,
    state: RefCell<State>,
    /// The SMU's reply arguments to a message.
    reply: fn(&Message) -> [u32; 6],
}
#[derive(Default)]
struct State {
    index: BTreeMap<u8, u32>,
    smn: BTreeMap<(u8, u32), u32>,
    messages: Vec<Message>,
}
impl Pci {
    pub fn new(buses: &[u8], reply: fn(&Message) -> [u32; 6]) -> Self {
        let pci = Self {
            buses: buses.into(),
            state: RefCell::default(),
            reply,
        };
        // Every SMU is ready.
        (0..buses.len() as u8).for_each(|die| pci.set_smn(die, MAILBOX.response.address(), 1));
        pci
    }
    pub fn dies(&self) -> Vec<Die<&Self>> {
        unsafe { dies(self) }.collect()
    }
    pub fn smn(&self, die: u8, address: impl Into<u32>) -> u32 {
        let address = address.into();
        self.state
            .borrow()
            .smn
            .get(&(die, address))
            .copied()
            .unwrap_or(0)
    }
    pub fn set_smn(&self, die: u8, address: impl Into<u32>, value: u32) {
        self.state.borrow_mut().smn.insert((die, address.into()), value);
    }
    pub fn messages(&self) -> Vec<Message> {
        self.state.borrow().messages.clone()
    }
    fn die_of(&self, bus: u8) -> u8 {
        self.buses
            .iter()
            .position(|&b| b == bus)
            .expect("not a root bus") as u8
    }
}
impl ConfigRegionAccess for &Pci {
    unsafe fn read(&self, address: PciAddress, offset: u16) -> u32 {
        match (address.bus(), address.device(), address.function(), offset) {
            (0, 0x18, 1, 0x204) => self.buses.len() as u32,
            (0, device @ 0x18..=0x1f, 0, 0x84) => u32::from(self.buses[usize::from(device - 0x18)]),
            (bus, 0, 0, 0xbc) => {
                let index = self.state.borrow().index[&bus];
                self.smn(self.die_of(bus), index)
            }
            other => panic!("unexpected PCI read {other:x?}"),
        }
    }
    unsafe fn write(&self, address: PciAddress, offset: u16, value: u32) {
        match (address.bus(), address.device(), address.function(), offset) {
            (bus, 0, 0, 0xb8) => {
                self.state.borrow_mut().index.insert(bus, value);
            }
            (bus, 0, 0, 0xbc) => {
                let die = self.die_of(bus);
                let index = self.state.borrow().index[&bus];
                self.set_smn(die, index, value);
                if index == MAILBOX.message.address() {
                    let message = Message {
                        die,
                        message: value,
                        arguments: core::array::from_fn(|i| {
                            self.smn(die, MAILBOX.argument(i).address())
                        }),
                    };
                    for (i, word) in (self.reply)(&message).into_iter().enumerate() {
                        self.set_smn(die, MAILBOX.argument(i).address(), word);
                    }
                    self.set_smn(die, MAILBOX.response.address(), 1);
                    self.state.borrow_mut().messages.push(message);
                }
            }
            other => panic!("unexpected PCI write {other:x?} = {value:#x}"),
        }
    }
}
