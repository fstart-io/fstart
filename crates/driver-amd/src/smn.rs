// SPDX-License-Identifier: MIT OR Apache-2.0
//! The SMN, the indirect register network of every AMD SoC since Family
//! 17h: typed register descriptors
//! ([`SmnReg`]) and the space that resolves them to tock registers
//! ([`Smn`]). The index/data pair is exclusively owned by the BSP during
//! silicon init.
//!
//! Every die has its own SMN space, reached through D0F0 0xb8/0xbc of the
//! die's root bus. Single-die parts (Raven/Picasso) use bus 0.
//!
//! ```ignore
//! smn.reg(IOAPIC_BASE_LO).write(IoapicBase::ADDRESS.val(base >> 8) + IoapicBase::ENABLE::SET);
//! ```
use crate::table;
use core::marker::PhantomData;
use fstart_core::indexed::Indexed;
use fstart_pci::{ConfigRegionAccess, ConfigRegister, PciAddress};
use tock_registers::RegisterLongName;


/// A 32-bit SMN register with the bitfields `R`. SMN is a chipset register
/// network, not an x86 physical address space.
pub struct SmnReg<R: RegisterLongName = ()> {
    address: u32,
    _reg: PhantomData<R>,
}
impl<R: RegisterLongName> Clone for SmnReg<R> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<R: RegisterLongName> Copy for SmnReg<R> {}
impl<R: RegisterLongName> SmnReg<R> {
    pub const fn new(address: u32) -> Self {
        assert!(address & 3 == 0);
        Self {
            address,
            _reg: PhantomData,
        }
    }
    pub const fn address(self) -> u32 {
        self.address
    }
    /// The same register `bytes` further on: the next instance of a
    /// replicated block.
    pub const fn offset(self, bytes: u32) -> Self {
        Self::new(self.address + bytes)
    }
}

impl<R: RegisterLongName> From<SmnReg<R>> for u32 {
    fn from(reg: SmnReg<R>) -> u32 {
        reg.address
    }
}

const SMN_INDEX: u16 = 0xb8;
const SMN_DATA: u16 = 0xbc;

/// An SMN register of a die as a tock register.
pub type SmnRegister<'a, P, R> = Indexed<ConfigRegister<&'a P>, ConfigRegister<&'a P>, R>;

/// The SMN space of one die.
pub struct Smn<P> {
    pci: P,
    root: PciAddress,
}
impl<P: ConfigRegionAccess> Smn<P> {
    /// SMN of the die whose root complex is on bus 0.
    /// # Safety
    /// PCI D0F0 exists and its B8/BC SMN index/data pair is exclusively owned.
    pub unsafe fn new(pci: P) -> Self {
        unsafe { Self::on_bus(pci, 0) }
    }
    /// SMN of the die whose root complex is on `bus`.
    /// # Safety
    /// As [`Self::new`], for that root bus.
    pub unsafe fn on_bus(pci: P, bus: u8) -> Self {
        Self {
            pci,
            root: PciAddress::new(0, bus, 0, 0),
        }
    }
    pub fn reg<R: RegisterLongName>(&self, reg: SmnReg<R>) -> SmnRegister<'_, P, R> {
        // SAFETY: the constructor's contract covers the index/data pair.
        let (index, data) = unsafe {
            (
                ConfigRegister::new(&self.pci, self.root, SMN_INDEX),
                ConfigRegister::new(&self.pci, self.root, SMN_DATA),
            )
        };
        Indexed::new(index, data, reg.address)
    }
}

// ---- Vendor register tables ([`crate::table`]) ----

/// A vendor SMN table record.
pub type Reg = table::Record<SmnReg, u32>;
pub const fn rmw(address: u32, clear: u32, set: u32) -> Reg {
    table::rmw(SmnReg::new(address), clear, set)
}
pub const fn write(address: u32, value: u32) -> Reg {
    table::write(SmnReg::new(address), value)
}
/// `SMN[address] = SMN[from]`
pub const fn copy(address: u32, from: u32) -> Reg {
    table::copy(SmnReg::new(address), SmnReg::new(from))
}
/// Apply a vendor SMN table in order.
pub fn apply(smn: &Smn<impl ConfigRegionAccess>, records: impl IntoIterator<Item = Reg>) {
    table::apply(records, |reg| smn.reg(reg));
}

#[cfg(any(test, feature = "testing"))]
pub mod testing {
    //! An SMN space for tests, behind the D0F0 index/data pair as the
    //! hardware has it.
    extern crate std;
    use super::*;
    use core::cell::{Cell, RefCell};
    use std::collections::BTreeMap;

    /// SMN registers of one die; unset registers read as zero. `on_write`
    /// models side effects (e.g. a mailbox answering).
    #[derive(Default)]
    pub struct SmnSpace {
        index: Cell<u32>,
        pub regs: RefCell<BTreeMap<u32, u32>>,
        pub on_write: Option<fn(&SmnSpace, u32, u32)>,
    }
    impl SmnSpace {
        pub fn get(&self, address: impl Into<u32>) -> u32 {
            self.regs.borrow().get(&address.into()).copied().unwrap_or(0)
        }
        pub fn set(&self, address: impl Into<u32>, value: u32) {
            self.regs.borrow_mut().insert(address.into(), value);
        }
        pub fn smn(&self) -> Smn<&Self> {
            // SAFETY: a host model.
            unsafe { Smn::new(self) }
        }
    }
    /// A test model of an SMN space with side effects, behind the index/data
    /// pair of [`SmnTest`].
    pub trait SmnModel {
        fn read(&mut self, address: u32) -> u32;
        fn write(&mut self, address: u32, value: u32);
    }
    pub struct SmnTest<M> {
        index: Cell<u32>,
        model: RefCell<M>,
    }
    impl<M: SmnModel> SmnTest<M> {
        pub fn new(model: M) -> Self {
            Self {
                index: Cell::new(0),
                model: RefCell::new(model),
            }
        }
        pub fn smn(&self) -> Smn<&Self> {
            // SAFETY: a host model.
            unsafe { Smn::new(self) }
        }
        pub fn model(&self) -> core::cell::RefMut<'_, M> {
            self.model.borrow_mut()
        }
    }
    impl<M: SmnModel> ConfigRegionAccess for SmnTest<M> {
        unsafe fn read(&self, address: PciAddress, offset: u16) -> u32 {
            assert_eq!((address, offset), (PciAddress::new(0, 0, 0, 0), SMN_DATA));
            self.model.borrow_mut().read(self.index.get())
        }
        unsafe fn write(&self, address: PciAddress, offset: u16, value: u32) {
            assert_eq!(address, PciAddress::new(0, 0, 0, 0));
            match offset {
                SMN_INDEX => self.index.set(value),
                SMN_DATA => self.model.borrow_mut().write(self.index.get(), value),
                _ => panic!("not the SMN pair: {offset:#x}"),
            }
        }
    }

    impl ConfigRegionAccess for SmnSpace {
        unsafe fn read(&self, address: PciAddress, offset: u16) -> u32 {
            assert_eq!((address, offset), (PciAddress::new(0, 0, 0, 0), SMN_DATA));
            self.get(self.index.get())
        }
        unsafe fn write(&self, address: PciAddress, offset: u16, value: u32) {
            assert_eq!(address, PciAddress::new(0, 0, 0, 0));
            match offset {
                SMN_INDEX => self.index.set(value),
                SMN_DATA => {
                    let index = self.index.get();
                    self.set(index, value);
                    if let Some(on_write) = self.on_write {
                        on_write(self, index, value);
                    }
                }
                _ => panic!("not the SMN pair: {offset:#x}"),
            }
        }
    }
}
