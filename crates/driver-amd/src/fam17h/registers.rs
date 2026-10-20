// SPDX-License-Identifier: MIT OR Apache-2.0
//! Family 17h indirect register spaces (SMN, Data Fabric):
//! typed register descriptors ([`SmnReg`], [`DfReg`]) and the spaces that
//! resolve them to tock registers ([`Smn`], [`Df`]). Index/data pairs are
//! exclusively owned by the BSP during silicon init.
//!
//! Every die has its own SMN space, reached through D0F0 0xb8/0xbc of the
//! die's root bus, and its own Data Fabric PCI device 0x18 + node on bus 0.
//! Single-die parts (Raven/Picasso) use node 0 on bus 0.
//!
//! ```ignore
//! smn.reg(IOAPIC_BASE_LO).write(IoapicBase::ADDRESS.val(base >> 8) + IoapicBase::ENABLE::SET);
//! df.instance(F3_CCM_108, INST_CCM0).modify(Ccm108::BIT6::SET);
//! ```
use core::marker::PhantomData;
use fstart_core::indexed::Indexed;
use fstart_pci::{ConfigRegionAccess, ConfigRegister, PciAddress};
use tock_registers::{
    RegisterLongName,
    interfaces::{Readable, Writeable},
    register_bitfields,
};

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

// ---- Vendor register tables ----
//
// Vendor sequences whose bits have no known meaning stay raw records, in
// their original order: repeated writes are part of the sequence.

/// One record of a vendor register table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// `reg = (reg & !clear) | set`
    Update {
        clear: u32,
        set: u32,
    },
    Write(u32),
    /// `reg = SMN[from]`
    Copy {
        from: u32,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reg {
    pub address: u32,
    pub op: Op,
}
pub const fn rmw(address: u32, clear: u32, set: u32) -> Reg {
    Reg {
        address,
        op: Op::Update { clear, set },
    }
}
pub const fn write(address: u32, value: u32) -> Reg {
    Reg {
        address,
        op: Op::Write(value),
    }
}
pub const fn copy(address: u32, from: u32) -> Reg {
    Reg {
        address,
        op: Op::Copy { from },
    }
}

/// Apply a vendor register table in order. The records are raw: the
/// meaning of most of their bits isn't known.
pub fn apply(smn: &Smn<impl ConfigRegionAccess>, table: impl IntoIterator<Item = Reg>) {
    for record in table {
        let reg = smn.reg(SmnReg::<()>::new(record.address));
        match record.op {
            Op::Update { clear, set } => reg.set((reg.get() & !clear) | set),
            Op::Write(value) => reg.set(value),
            Op::Copy { from } => reg.set(smn.reg(SmnReg::<()>::new(from)).get()),
        }
    }
}

register_bitfields![u32,
    Ficaa [INSTANCE_ACCESS OFFSET(0) NUMBITS(1) [], REGISTER OFFSET(2) NUMBITS(9) [],
        FUNCTION OFFSET(11) NUMBITS(3) [], INSTANCE OFFSET(16) NUMBITS(8) []]
];

/// A 32-bit Data Fabric register (function, offset) with the bitfields `R`.
pub struct DfReg<R: RegisterLongName = ()> {
    function: u8,
    offset: u16,
    _reg: PhantomData<R>,
}
impl<R: RegisterLongName> Clone for DfReg<R> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<R: RegisterLongName> Copy for DfReg<R> {}
impl<R: RegisterLongName> DfReg<R> {
    pub const fn new(function: u8, offset: u16) -> Self {
        assert!(function < 8 && offset < 2048 && offset & 3 == 0);
        Self {
            function,
            offset,
            _reg: PhantomData,
        }
    }
    /// The register `bytes` further on: the next pair of a register array.
    pub const fn offset(self, bytes: u16) -> Self {
        Self::new(self.function, self.offset + bytes)
    }
}

/// A Data Fabric register as a tock register: either the node's own
/// register in its config space or one instance's through FICAA/FICAD.
pub enum DfRegister<'a, P: ConfigRegionAccess, R: RegisterLongName> {
    Own(ConfigRegister<&'a P, R>),
    Instance(Indexed<ConfigRegister<&'a P>, ConfigRegister<&'a P>, R>),
}
impl<P: ConfigRegionAccess, R: RegisterLongName> Readable for DfRegister<'_, P, R> {
    type T = u32;
    type R = R;
    fn get(&self) -> u32 {
        match self {
            Self::Own(register) => register.get(),
            Self::Instance(register) => register.get(),
        }
    }
}
impl<P: ConfigRegionAccess, R: RegisterLongName> Writeable for DfRegister<'_, P, R> {
    type T = u32;
    type R = R;
    fn set(&self, value: u32) {
        match self {
            Self::Own(register) => register.set(value),
            Self::Instance(register) => register.set(value),
        }
    }
}

const FICAA: u16 = 0x5c;
const FICAD_LO: u16 = 0x98;

/// The Data Fabric of one node.
pub struct Df<P> {
    pci: P,
    device: u8,
}
impl<P: ConfigRegionAccess> Df<P> {
    /// Data Fabric of node 0.
    /// # Safety
    /// D18F0..F7 exist; the FICAA/FICAD pair is exclusively owned by this caller.
    pub unsafe fn new(pci: P) -> Self {
        unsafe { Self::for_node(pci, 0) }
    }
    /// Data Fabric of `node` (PCI device 0x18 + node on bus 0).
    /// # Safety
    /// As [`Self::new`], for that node's device.
    pub unsafe fn for_node(pci: P, node: u8) -> Self {
        assert!(node < 8);
        Self {
            pci,
            device: 0x18 + node,
        }
    }
    /// The node's own register, what AGESA calls a broadcast access.
    pub fn reg<R: RegisterLongName>(&self, reg: DfReg<R>) -> DfRegister<'_, P, R> {
        let address = PciAddress::new(0, 0, self.device, reg.function);
        // SAFETY: the constructor's contract.
        DfRegister::Own(unsafe { ConfigRegister::new(&self.pci, address, reg.offset) })
    }
    /// The register of fabric instance `instance`, through FICAA/FICAD.
    pub fn instance<R: RegisterLongName>(
        &self,
        reg: DfReg<R>,
        instance: u8,
    ) -> DfRegister<'_, P, R> {
        let df4 = PciAddress::new(0, 0, self.device, 4);
        let ficaa = Ficaa::INSTANCE_ACCESS::SET
            + Ficaa::REGISTER.val(u32::from(reg.offset >> 2))
            + Ficaa::FUNCTION.val(u32::from(reg.function))
            + Ficaa::INSTANCE.val(u32::from(instance));
        // SAFETY: the constructor's contract covers the FICAA/FICAD pair.
        let (index, data) = unsafe {
            (
                ConfigRegister::new(&self.pci, df4, FICAA),
                ConfigRegister::new(&self.pci, df4, FICAD_LO),
            )
        };
        DfRegister::Instance(Indexed::new(index, data, ficaa.value))
    }
}

/// One record of a vendor Data Fabric table: `reg = (reg & !clear) | set`
/// on the node's own register (`instance: None`) or one fabric instance's.
/// A clear mask of all ones writes `set` without reading.
#[derive(Clone, Copy)]
pub struct DfRecord {
    pub reg: DfReg,
    pub instance: Option<u8>,
    pub clear: u32,
    pub set: u32,
}
pub const fn df_rmw(reg: DfReg, instance: Option<u8>, clear: u32, set: u32) -> DfRecord {
    DfRecord {
        reg,
        instance,
        clear,
        set,
    }
}
pub const fn df_write(reg: DfReg, instance: Option<u8>, value: u32) -> DfRecord {
    df_rmw(reg, instance, u32::MAX, value)
}

/// Apply a vendor Data Fabric table in order.
pub fn apply_df(df: &Df<impl ConfigRegionAccess>, table: impl IntoIterator<Item = DfRecord>) {
    for record in table {
        let reg = match record.instance {
            None => df.reg(record.reg),
            Some(instance) => df.instance(record.reg, instance),
        };
        if record.clear == u32::MAX {
            reg.set(record.set);
        } else {
            reg.set((reg.get() & !record.clear) | record.set);
        }
    }
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

    /// The Data Fabric of node 0 for tests: registers by (function,
    /// offset, instance), `None` for the node's own; writes are logged.
    /// (function, offset, instance).
    pub type DfKey = (u8, u16, Option<u8>);
    #[derive(Default)]
    pub struct DfSpace {
        ficaa: Cell<u32>,
        pub regs: RefCell<BTreeMap<DfKey, u32>>,
        pub writes: RefCell<std::vec::Vec<(DfKey, u32)>>,
    }
    impl DfSpace {
        pub fn df(&self) -> Df<&Self> {
            // SAFETY: a host model.
            unsafe { Df::new(self) }
        }
        fn target(&self, address: PciAddress, offset: u16) -> DfKey {
            assert_eq!((address.bus(), address.device()), (0, 0x18));
            if (address.function(), offset) == (4, FICAD_LO) {
                let ficaa = tock_registers::LocalRegisterCopy::<u32, Ficaa::Register>::new(
                    self.ficaa.get(),
                );
                assert!(ficaa.is_set(Ficaa::INSTANCE_ACCESS));
                let register = (ficaa.read(Ficaa::REGISTER) << 2) as u16;
                let function = ficaa.read(Ficaa::FUNCTION) as u8;
                return (function, register, Some(ficaa.read(Ficaa::INSTANCE) as u8));
            }
            (address.function(), offset, None)
        }
    }
    impl ConfigRegionAccess for DfSpace {
        unsafe fn read(&self, address: PciAddress, offset: u16) -> u32 {
            let target = self.target(address, offset);
            self.regs.borrow().get(&target).copied().unwrap_or(0)
        }
        unsafe fn write(&self, address: PciAddress, offset: u16, value: u32) {
            if (address.function(), offset) == (4, FICAA) {
                self.ficaa.set(value);
                return;
            }
            let key = self.target(address, offset);
            self.regs.borrow_mut().insert(key, value);
            self.writes.borrow_mut().push((key, value));
        }
    }
}
