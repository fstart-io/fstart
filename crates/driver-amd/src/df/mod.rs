// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Data Fabric: typed
//! register descriptors ([`DfReg`]) and the space that resolves them to tock
//! registers ([`Df`]), either the node's own registers or a fabric
//! instance's through the FICAA/FICAD pair, which the BSP owns exclusively
//! during silicon init.
//!
//! Every die has its own Data Fabric PCI device 0x18 + node on bus 0
//! ([`Die`]). Single-die parts (Raven/Picasso) use node 0. The MMIO, I/O
//! and bus split over the root bridges is [`plan`], its writers
//! [`program`], and the per-version layout of those rules [`Version`]
//! ([`v2::Df2`]; DF3 is SP3's). [`map`] reads back what is routed; the init
//! sequences of the SoCs' fabrics are [`picasso`] (AGESA AmdFabricRv) and
//! [`zeppelin`] (AmdFabricZp).
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

mod die;
pub mod map;
pub mod picasso;
pub mod plan;
pub mod program;
pub mod regs;
pub mod root;
pub mod v2;
pub mod zeppelin;

pub use die::{Die, die_count, dies, root_bridges};

/// Most root bridges a system has: 2 sockets × 4 (Naples dies, Rome/Milan
/// NBIO quadrants).
pub const MAX_ROOT_BRIDGES: usize = 8;

/// One PCI root bridge: a ZP die's NBIO (Naples) or an IOMS/NBIO quadrant
/// (Rome, Milan).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RootBridge {
    pub socket: u8,
    /// Die (Naples) or NBIO quadrant / IOMS index (Rome, Milan) within the
    /// socket.
    pub index: u8,
    /// DF fabric ID of the IOMS, as DstFabricID in the address maps.
    pub fabric_id: u16,
    /// First bus routed to this root bridge.
    pub bus_base: u8,
    /// Last bus routed to this root bridge.
    pub bus_limit: u8,
    /// The root bridge with the FCH (legacy IO/MMIO, VGA by default).
    pub primary: bool,
}

impl RootBridge {
    /// Physical index over the system: `socket * per_socket + index`.
    pub const fn physical(&self, per_socket: usize) -> usize {
        self.socket as usize * per_socket + self.index as usize
    }
}

/// An inclusive bus range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BusRange {
    pub base: u8,
    pub limit: u8,
}

impl BusRange {
    pub const fn count(&self) -> u16 {
        self.limit as u16 - self.base as u16 + 1
    }
}

/// A decoded DRAM address rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DramRule {
    /// First byte.
    pub base: u64,
    /// One past the last byte.
    pub end: u64,
    /// DstFabricID of the rule (the CS, or the first CS of an interleave).
    pub dst: u16,
    /// The rule's range has the MMIO hole (DramHoleControl) cut out.
    pub hole: bool,
    /// Interleaved across sockets.
    pub socket_interleave: bool,
    /// Die interleave field (DF2 `limit[11:10]`, DF3 `base[7:6]`).
    pub die_interleave: u8,
    /// Channel interleave encoding (DF2 `base[7:4]`, DF3 `base[5:2]`).
    pub channel_interleave: u8,
}

/// The per-version DF layout: DstFabricID widths and the fields that moved
/// between DF2 and DF3 (SOC_DF §3.1, §3.4.4).
pub trait Version {
    /// MmioAddress rule triples.
    const MMIO_PAIRS: u8 = 16;
    /// X86IO rule pairs.
    const IO_PAIRS: u8 = 8;
    /// CfgAddressMap registers.
    const CFG_MAPS: u8 = 8;
    /// DramBase/LimitAddress rules.
    const DRAM_RULES: u8 = 16;

    type MmioCtrl: RegisterLongName;
    type IoLimit: RegisterLongName;
    type CfgMap: RegisterLongName;
    type VgaEn: RegisterLongName;

    /// MmioAddressControl routing to `dst`, RE and WE set, NP clear.
    fn mmio_ctrl(dst: u16) -> FieldValue<u32, Self::MmioCtrl>;
    /// X86IOLimitAddress: `limit` is the last port, routed to `dst`.
    fn io_limit(limit: u32, dst: u16) -> FieldValue<u32, Self::IoLimit>;
    /// CfgAddressMap routing `bus` to `dst`, RE and WE set.
    fn cfg_map(bus: BusRange, dst: u16) -> FieldValue<u32, Self::CfgMap>;
    /// Decode an enabled (RE and WE) CfgAddressMap.
    fn decode_cfg_map(value: u32) -> Option<(BusRange, u16)>;
    /// Decode a valid DRAM rule.
    fn decode_dram(base: u32, limit: u32) -> Option<DramRule>;
    /// VGAEn routing the legacy VGA window to `dst`, enabled, CPU access on.
    fn vga(dst: u16) -> FieldValue<u32, Self::VgaEn>;

    fn mmio_ctrl_reg(pair: u8) -> DfReg<Self::MmioCtrl> {
        DfReg::new(0, regs::MMIO_CTRL_OFFSET).at(pair, regs::MMIO_STRIDE)
    }
    fn io_limit_reg(pair: u8) -> DfReg<Self::IoLimit> {
        DfReg::new(0, regs::IO_LIMIT_OFFSET).at(pair, regs::IO_STRIDE)
    }
    fn cfg_map_reg(map: u8) -> DfReg<Self::CfgMap> {
        DfReg::new(0, regs::CFG_MAP_OFFSET).at(map, regs::CFG_MAP_STRIDE)
    }
    fn vga_reg() -> DfReg<Self::VgaEn> {
        DfReg::new(0, regs::VGA_EN_OFFSET)
    }
}

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

/// Most sockets a fabric connects.
pub const MAX_SOCKETS: usize = 2;

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
