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
use tock_registers::interfaces::{Readable, Writeable};

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
    /// The same register `bytes` further on, a multiple of 4: the next
    /// instance of a replicated block. No panic path at run time, which
    /// an SMM image may not link.
    pub const fn offset(self, bytes: u32) -> Self {
        Self {
            address: self.address.wrapping_add(bytes & !3),
            _reg: PhantomData,
        }
    }
}

impl<R: RegisterLongName> From<SmnReg<R>> for u32 {
    fn from(reg: SmnReg<R>) -> u32 {
        reg.address
    }
}

/// D0F0 SMN index register.
pub const SMN_INDEX: u16 = 0xb8;
/// D0F0 SMN data register.
pub const SMN_DATA: u16 = 0xbc;

/// An SMN address (dword aligned for register accesses).
pub type SmnAddr = u32;

/// An SMN register of a die as a tock register.
pub type SmnRegister<'a, P, R> = Indexed<ConfigRegister<&'a P>, ConfigRegister<&'a P>, R>;

/// 00.0 of `bus`, without `PciAddress::new`: its device/function range
/// asserts are a panic path, which an SMM image reaching SMN may not link,
/// and 00.0 has nothing to check.
fn host_bridge(bus: u8) -> PciAddress {
    const { assert!(size_of::<PciAddress>() == size_of::<u32>()) };
    // SAFETY: `PciAddress` is one `u32` packed segment:bus:device:function
    // = 16:8:5:3 (pci_types docs; same size checked above, the test below
    // pins the packing), and every `u32` is a valid address.
    unsafe { core::mem::transmute::<u32, PciAddress>(u32::from(bus) << 8) }
}

/// The SMN space of one die (Naples, Zeppelin, Picasso) or IOD (Rome,
/// Milan), through the host bridge of one of its root buses.
#[derive(Clone, Copy)]
pub struct Smn<P> {
    pci: P,
    root: PciAddress,
}
impl<P: ConfigRegionAccess> Smn<P> {
    /// SMN of the die whose root complex is on bus 0.
    /// # Safety
    /// PCI D0F0 exists and its B8/BC SMN index/data pair is exclusively owned
    /// while the value is used (the BSP during boot; SMM saves and restores
    /// the index, [`Self::with_saved_index`]). The holder only accesses
    /// registers it owns, with the side effects it means: SMN reaches every
    /// IP block of the die.
    pub unsafe fn new(pci: P) -> Self {
        unsafe { Self::on_bus(pci, 0) }
    }
    /// SMN of the die whose root complex is on `bus`.
    /// # Safety
    /// As [`Self::new`], for that root bus.
    pub unsafe fn on_bus(pci: P, bus: u8) -> Self {
        Self {
            pci,
            root: host_bridge(bus),
        }
    }
    /// The root bus whose host bridge carries the accesses.
    pub fn bus(&self) -> u8 {
        self.root.bus()
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
    /// The dword at `address`, for the vendor tables that name registers
    /// by address only.
    pub fn read32(&self, address: SmnAddr) -> u32 {
        // SAFETY: the constructor's contract covers the index/data pair.
        unsafe {
            self.pci.write(self.root, SMN_INDEX, address & !3);
            self.pci.read(self.root, SMN_DATA)
        }
    }
    pub fn write32(&self, address: SmnAddr, value: u32) {
        // SAFETY: the constructor's contract covers the index/data pair.
        unsafe {
            self.pci.write(self.root, SMN_INDEX, address & !3);
            self.pci.write(self.root, SMN_DATA, value)
        }
    }
    /// Clear `clear`, then set `set`.
    pub fn modify32(&self, address: SmnAddr, clear: u32, set: u32) {
        self.write32(address, (self.read32(address) & !clear) | set)
    }
    /// Run `access` and restore the index register: SMM may interrupt the
    /// OS between its index and data accesses. Each socket has its own
    /// index to preserve. `access` must not unwind (firmware aborts on
    /// panic): the index would stay changed.
    pub fn with_saved_index<T>(&self, access: impl FnOnce(&Self) -> T) -> T {
        // SAFETY: the constructor's contract covers the index/data pair.
        let saved = unsafe { self.pci.read(self.root, SMN_INDEX) };
        let result = access(self);
        // SAFETY: as above.
        unsafe { self.pci.write(self.root, SMN_INDEX, saved) };
        result
    }
}

/// A view of an SMN space the IP blocks program through: an [`Smn`], or
/// one that relocates and filters the registers it hands out (an SP3 NBIO
/// quadrant).
pub trait SmnAccess {
    type Reg<'a, R: RegisterLongName>: Readable<T = u32, R = R> + Writeable<T = u32, R = R>
    where
        Self: 'a;
    fn reg<R: RegisterLongName>(&self, reg: SmnReg<R>) -> Self::Reg<'_, R>;
}
impl<P: ConfigRegionAccess> SmnAccess for Smn<P> {
    type Reg<'a, R: RegisterLongName>
        = SmnRegister<'a, P, R>
    where
        Self: 'a;
    fn reg<R: RegisterLongName>(&self, reg: SmnReg<R>) -> Self::Reg<'_, R> {
        Smn::reg(self, reg)
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
            self.regs
                .borrow()
                .get(&address.into())
                .copied()
                .unwrap_or(0)
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

#[cfg(test)]
pub(crate) mod model {
    //! A config-space model with an SMN behind every host bridge, for tests.
    extern crate std;
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::rc::Rc;

    use fstart_pci::{ConfigRegionAccess, PciAddress};

    use super::{SMN_DATA, SMN_INDEX};

    /// Shared state: config dwords and SMN dwords per bus.
    #[derive(Default)]
    pub struct State {
        pub config: BTreeMap<(u8, u8, u8, u16), u32>,
        pub smn: BTreeMap<(u8, u32), u32>,
        pub index: BTreeMap<u8, u32>,
        pub log: std::vec::Vec<(u8, u32, u32)>,
    }

    /// Cloneable handle on the model.
    #[derive(Clone, Default)]
    pub struct Model(pub Rc<RefCell<State>>);

    impl ConfigRegionAccess for Model {
        unsafe fn read(&self, a: PciAddress, offset: u16) -> u32 {
            let s = self.0.borrow();
            if (a.device(), a.function(), offset) == (0, 0, SMN_INDEX) {
                return s.index.get(&a.bus()).copied().unwrap_or(0);
            }
            if (a.device(), a.function(), offset) == (0, 0, SMN_DATA) {
                let index = s.index.get(&a.bus()).copied().unwrap_or(0);
                return s.smn.get(&(a.bus(), index)).copied().unwrap_or(0);
            }
            let key = (a.bus(), a.device(), a.function(), offset);
            s.config.get(&key).copied().unwrap_or(0xffff_ffff)
        }

        unsafe fn write(&self, a: PciAddress, offset: u16, value: u32) {
            let mut s = self.0.borrow_mut();
            match (a.device(), a.function(), offset) {
                (0, 0, SMN_INDEX) => {
                    s.index.insert(a.bus(), value);
                }
                (0, 0, SMN_DATA) => {
                    let index = s.index.get(&a.bus()).copied().unwrap_or(0);
                    s.smn.insert((a.bus(), index), value);
                    s.log.push((a.bus(), index, value));
                }
                _ => {
                    s.config
                        .insert((a.bus(), a.device(), a.function(), offset), value);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::model::Model;
    use super::*;
    use tock_registers::interfaces::{Readable, Writeable};
    use tock_registers::register_bitfields;

    register_bitfields![u32, TEST [LOW OFFSET(0) NUMBITS(4) [], HIGH OFFSET(28) NUMBITS(4) []]];

    #[test]
    fn host_bridge_is_device_0_function_0_of_the_bus() {
        assert!((0..=u8::MAX).all(|bus| host_bridge(bus) == PciAddress::new(0, bus, 0, 0)));
    }

    #[test]
    fn index_data_round_trip() {
        let model = Model::default();
        let smn = unsafe { Smn::on_bus(model.clone(), 0x40) };
        smn.write32(0x13b1_02e2, 0x1234);
        assert_eq!(smn.read32(0x13b1_02e0), 0x1234, "index is dword aligned");
        smn.modify32(0x13b1_02e0, 0x0f00, 0x8000_0000);
        assert_eq!(model.0.borrow().smn[&(0x40, 0x13b1_02e0)], 0x8000_1034);
    }

    #[test]
    fn preserves_each_socket_index_without_touching_the_other_socket() {
        let model = Model::default();
        model
            .0
            .borrow_mut()
            .index
            .extend([(0, 0x1110), (0xa0, 0x2220)]);
        model.0.borrow_mut().smn.insert((0, 0x1d840), 0x1234);
        let remote = unsafe { Smn::on_bus(model.clone(), 0xa0) };
        remote.with_saved_index(|s| {
            s.write32(0x1d840, 0);
            s.write32(0x1d848, 0xdead0000);
            s.write32(0x1d84c, 0x42);
            s.write32(0x1d840, 7);
        });
        let state = model.0.borrow();
        assert_eq!(state.index[&0], 0x1110);
        assert_eq!(state.index[&0xa0], 0x2220);
        assert_eq!(state.smn[&(0, 0x1d840)], 0x1234);
        assert_eq!(
            state.log,
            [
                (0xa0, 0x1d840, 0),
                (0xa0, 0x1d848, 0xdead0000),
                (0xa0, 0x1d84c, 0x42),
                (0xa0, 0x1d840, 7)
            ]
        );
    }

    #[test]
    fn typed_register() {
        let model = Model::default();
        let smn = unsafe { Smn::new(model.clone()) };
        let reg = smn.reg(SmnReg::<TEST::Register>::new(0x100));
        reg.write(TEST::LOW.val(5) + TEST::HIGH.val(0xa));
        assert_eq!(reg.read(TEST::HIGH), 0xa);
        assert_eq!(reg.get(), 0xa000_0005);
    }
}
