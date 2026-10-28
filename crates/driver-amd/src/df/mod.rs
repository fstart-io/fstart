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
use fstart_pci::{ConfigRegionAccess, ConfigRegister, PciAddress};
use tock_registers::{
    RegisterLongName,
    fields::FieldValue,
    interfaces::{Readable, Writeable},
    register_bitfields,
};

#[cfg(any(feature = "picasso", feature = "zeppelin"))]
mod die;
#[cfg(any(feature = "picasso", feature = "zeppelin"))]
pub mod map;
#[cfg(feature = "picasso")]
pub mod picasso;
pub mod regs;
pub mod v2;
#[cfg(feature = "zeppelin")]
pub mod zeppelin;

#[cfg(any(feature = "picasso", feature = "zeppelin"))]
pub use die::{Die, die_count, dies};

register_bitfields![u32,
    /// F4x5C FabricIndirectConfigAccessAddress (set 3, the one the x86 may
    /// use), issued on the target node.
    pub Ficaa [
        INSTANCE_ACCESS OFFSET(0) NUMBITS(1) [],
        REGISTER OFFSET(2) NUMBITS(9) [],
        FUNCTION OFFSET(11) NUMBITS(3) [],
        WIDTH_64 OFFSET(14) NUMBITS(1) [],
        INSTANCE OFFSET(16) NUMBITS(8) []
    ]
];

/// F4x5C FICAA and F4x98 FICAD_LO.
pub const FICAA: u16 = 0x5c;
pub const FICAD_LO: u16 = 0x98;
/// PCI device of DF node 0 on bus 0; node `n` is `0x18 + n`.
pub const NODE0_DEVICE: u8 = 0x18;
/// Most DF nodes: 2 sockets of 4 ZP dies.
pub const MAX_NODES: u8 = 8;

/// Function `function` of DF `node`: bus 0, device `0x18 + node`.
pub fn node_address(node: u8, function: u8) -> PciAddress {
    PciAddress::new(0, 0, NODE0_DEVICE + node, function)
}

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
    /// Checked in debug builds only: the SMM RAS handler builds registers
    /// at run time and must not link a panic.
    pub const fn new(function: u8, offset: u16) -> Self {
        debug_assert!(function < 8 && offset < 2048 && offset & 3 == 0);
        Self {
            function,
            offset,
            _reg: PhantomData,
        }
    }
    /// The register without its bitfields, as the vendor tables take it.
    pub const fn erase(self) -> DfReg {
        DfReg::new(self.function, self.offset)
    }
    /// The `index`th copy of an array register with `stride` bytes.
    pub const fn at(self, index: u8, stride: u16) -> Self {
        Self::new(self.function, self.offset + index as u16 * stride)
    }
    pub const fn function(self) -> u8 {
        self.function
    }
    /// The byte offset in the function's config space.
    pub const fn register(self) -> u16 {
        self.offset
    }
}

/// The FICAA value selecting `reg` of fabric instance `instance`; with
/// `targeted` clear, the access is AGESA's indirect broadcast.
pub fn ficaa<R: RegisterLongName>(
    reg: DfReg<R>,
    instance: u8,
    targeted: bool,
) -> FieldValue<u32, Ficaa::Register> {
    Ficaa::INSTANCE_ACCESS.val(u32::from(targeted))
        + Ficaa::REGISTER.val(u32::from(reg.offset >> 2))
        + Ficaa::FUNCTION.val(u32::from(reg.function))
        + Ficaa::INSTANCE.val(u32::from(instance))
}

/// Where an access goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// The node's own register (AGESA's "broadcast": a config write reaches
    /// every instance of the node, a read returns the node's copy).
    Node,
    /// One fabric instance of the node, through FICAA/FICAD.
    Instance(u8),
}

/// A Data Fabric register of one node or fabric instance as a tock
/// register.
pub struct DfRegister<P, R: RegisterLongName = ()> {
    pci: P,
    node: u8,
    target: Target,
    reg: DfReg<R>,
}
impl<P: ConfigRegionAccess, R: RegisterLongName> DfRegister<P, R> {
    /// `reg` of DF `node` at `target`.
    ///
    /// # Safety
    /// The node exists and the caller owns its FICAA/FICAD pair while
    /// instance accesses run (shared state, as SMN's index/data).
    pub unsafe fn new(pci: P, node: u8, target: Target, reg: DfReg<R>) -> Self {
        debug_assert!(node < MAX_NODES);
        Self {
            pci,
            node,
            target,
            reg,
        }
    }
    fn config<S: RegisterLongName>(&self, function: u8, offset: u16) -> ConfigRegister<&P, S> {
        // SAFETY: the constructor's contract.
        unsafe { ConfigRegister::new(&self.pci, node_address(self.node, function), offset) }
    }
    /// The data register; programs FICAA first for an instance access.
    fn data(&self) -> ConfigRegister<&P, R> {
        match self.target {
            Target::Node => self.config(self.reg.function, self.reg.offset),
            Target::Instance(instance) => {
                self.config::<Ficaa::Register>(4, FICAA)
                    .write(ficaa(self.reg, instance, true));
                self.config(4, FICAD_LO)
            }
        }
    }
}
impl<P: ConfigRegionAccess, R: RegisterLongName> Readable for DfRegister<P, R> {
    type T = u32;
    type R = R;
    fn get(&self) -> u32 {
        self.data().get()
    }
}
impl<P: ConfigRegionAccess, R: RegisterLongName> Writeable for DfRegister<P, R> {
    type T = u32;
    type R = R;
    fn set(&self, value: u32) {
        self.data().set(value)
    }
}

/// The Data Fabric of one node.
pub struct Df<P> {
    pci: P,
    node: u8,
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
        assert!(node < MAX_NODES);
        Self { pci, node }
    }
    /// The node's own register, what AGESA calls a broadcast access.
    pub fn reg<R: RegisterLongName>(&self, reg: DfReg<R>) -> DfRegister<&P, R> {
        // SAFETY: the constructor's contract.
        unsafe { DfRegister::new(&self.pci, self.node, Target::Node, reg) }
    }
    /// The register of fabric instance `instance`, through FICAA/FICAD.
    pub fn instance<R: RegisterLongName>(&self, reg: DfReg<R>, instance: u8) -> DfRegister<&P, R> {
        // SAFETY: the constructor's contract covers the FICAA/FICAD pair.
        unsafe { DfRegister::new(&self.pci, self.node, Target::Instance(instance), reg) }
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
