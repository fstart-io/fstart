// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Data Fabric: typed
//! register descriptors ([`DfReg`]) and the space that resolves them to tock
//! registers ([`Df`]), either the node's own registers or a fabric
//! instance's through the FICAA/FICAD pair, which the BSP owns exclusively
//! during silicon init.
//!
//! Every die has its own Data Fabric PCI device 0x18 + node on bus 0
//! ([`Die`]). Single-die parts (Raven/Picasso) use node 0. The address map
//! and its resource split are [`map`]; the init sequences of the SoCs'
//! fabrics are [`picasso`] (AGESA AmdFabricRv) and [`zeppelin`]
//! (AmdFabricZp).
//!
//! ```ignore
//! df.instance(F3_CCM_108, INST_CCM0).modify(Ccm108::BIT6::SET);
//! ```
use crate::table;
use core::marker::PhantomData;
use fstart_core::indexed::Indexed;
use fstart_pci::{ConfigRegionAccess, ConfigRegister, PciAddress};
use tock_registers::{
    RegisterLongName,
    interfaces::{Readable, Writeable},
    register_bitfields,
};

mod die;
pub mod map;
#[cfg(feature = "picasso")]
pub mod picasso;
#[cfg(feature = "zeppelin")]
pub mod zeppelin;

pub use die::{Die, die_count, dies};

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

/// A vendor Data Fabric table record: the node's own register
/// (`None`) or one fabric instance's.
pub type DfRecord = table::Record<(DfReg, Option<u8>), u32>;
pub const fn df_rmw(reg: DfReg, instance: Option<u8>, clear: u32, set: u32) -> DfRecord {
    table::rmw((reg, instance), clear, set)
}
pub const fn df_write(reg: DfReg, instance: Option<u8>, value: u32) -> DfRecord {
    table::write((reg, instance), value)
}

/// Apply a vendor Data Fabric table in order.
pub fn apply_df(df: &Df<impl ConfigRegionAccess>, records: impl IntoIterator<Item = DfRecord>) {
    table::apply(records, |(reg, instance)| match instance {
        None => df.reg(reg),
        Some(instance) => df.instance(reg, instance),
    });
}

#[cfg(any(test, feature = "testing"))]
pub mod testing {
    //! A Data Fabric for tests, behind the FICAA/FICAD pair as the hardware
    //! has it.
    extern crate std;
    use super::*;
    use core::cell::{Cell, RefCell};
    use std::collections::BTreeMap;


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
