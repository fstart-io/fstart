//! Data fabric: the DF2 (Naples) and DF3 (Rome, Milan) register layers,
//! topology discovery, the MMIO/IO/bus resource planner and its
//! programmers, and the per-generation DF init (SOC_DF).
//!
//! | Module | Content | Report |
//! |---|---|---|
//! | this file | node/function transport, FICAA/FICAD instance access, registers shared by DF2 and DF3 | SOC_DF §3.1 |
//! | [`v2`], [`v3`] | per-version bitfields ([`Version`]) | SOC_DF §3.1, §3.4.4 |
//! | [`topology`] | sockets, nodes, root bridges, logical order, Milan IOMS fuse | SOC_DF §3.1, §3.9 |
//! | [`plan`] | the pure resource planner (equal split, board overrides) | SOC_DF §3.4 |
//! | [`program`] | MMIO/IO/bus map writers, VGA, ReadyToBoot | SOC_DF §3.4.4, §3.7 |
//! | [`dram`] | DRAM map read-back, NPS → NUMA domains | SOC_DF §3.6–3.7, ACPI_SMM §3.5 |
//! | `naples`, `rome`, `milan` | fabric init per generation | SOC_DF §3.6, DF_ZP (SP4r2) |
//!
//! Register names follow the public PPR/Linux/illumos names where they exist
//! (`CfgAddressMap`, `MmioAddressControl`, ...). Registers that only appear
//! as values in the vendor code keep their `FnxOff` name.

use core::marker::PhantomData;

use fstart_pci::{ConfigRegionAccess, ConfigRegister, PciAddress};
use tock_registers::fields::FieldValue;
use tock_registers::interfaces::{Readable, Writeable};
use tock_registers::{RegisterLongName, register_bitfields};

pub mod dram;
pub mod plan;
pub mod program;
pub mod topology;
#[cfg(feature = "naples")]
pub mod v2;
#[cfg(any(feature = "rome", feature = "milan"))]
pub mod v3;

#[cfg(feature = "milan")]
pub mod milan;
#[cfg(feature = "naples")]
pub mod naples;
#[cfg(feature = "rome")]
pub mod rome;

#[cfg(test)]
#[allow(dead_code)]
pub(crate) mod model;

/// PCI device of DF node 0 on bus 0; node `n` is `0x18 + n`.
pub const NODE0_DEVICE: u8 = 0x18;
/// Most DF nodes on SP3: 2 sockets × 4 ZP dies.
pub const MAX_NODES: u8 = 8;

/// A DF register: function and offset, with the bitfields `R`.
pub struct Reg<R: RegisterLongName = ()> {
    pub func: u8,
    pub offset: u16,
    _reg: PhantomData<R>,
}

impl<R: RegisterLongName> Clone for Reg<R> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<R: RegisterLongName> Copy for Reg<R> {}

impl<R: RegisterLongName> Reg<R> {
    pub const fn new(func: u8, offset: u16) -> Self {
        Self {
            func,
            offset,
            _reg: PhantomData,
        }
    }

    /// The `index`th copy of an array register with `stride` bytes.
    pub const fn at(self, index: u8, stride: u16) -> Self {
        Self::new(self.func, self.offset + index as u16 * stride)
    }
}

/// Where an access goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// The node's own register (AGESA's "broadcast": a config write reaches
    /// every instance of the node, a read returns the node's copy).
    Node,
    /// One fabric instance of the node, through FICAA/FICAD (F4x5C/F4x98).
    Instance(u8),
}

register_bitfields![u32,
    /// F4x5C FabricIndirectConfigAccessAddress (set 3, the one the x86 may
    /// use). Issued on the target node (SOC_DF §3.1, IC).
    pub FICAA [
        TARGET_INST OFFSET(0) NUMBITS(1) [],
        REG OFFSET(2) NUMBITS(9) [],
        FUNC OFFSET(11) NUMBITS(3) [],
        WIDTH_64 OFFSET(14) NUMBITS(1) [],
        INST OFFSET(16) NUMBITS(8) []
    ],
    /// F0x40 FabricBlockInstanceCount.
    pub FBICNT [
        COUNT OFFSET(0) NUMBITS(8) []
    ],
    /// F0x84 CfgAddressCntl: the node's (or IOMS instance's) first bus.
    pub CFG_ADDR_CTL [
        BUS_NUM OFFSET(0) NUMBITS(8) []
    ],
    /// F0xC0+8i X86IOBaseAddress.
    pub IO_BASE [
        RE OFFSET(0) NUMBITS(1) [],
        WE OFFSET(1) NUMBITS(1) [],
        IE OFFSET(5) NUMBITS(1) [],
        BASE OFFSET(12) NUMBITS(13) []
    ],
    /// F0x104 DramHoleControl.
    pub DRAM_HOLE [
        VALID OFFSET(0) NUMBITS(1) [],
        BASE OFFSET(24) NUMBITS(8) []
    ],
    /// F1x204 SystemComponentCnt.
    pub COMPONENT_CNT [
        PIE OFFSET(0) NUMBITS(8) [],
        GCM OFFSET(8) NUMBITS(8) [],
        IOMS OFFSET(16) NUMBITS(8) []
    ],
    /// F4x44: bit 0 is what AGESA's ReadyToBoot handler calls "extended
    /// configuration access" (SOC_DF §3.7, all generations).
    pub CFG_ACCESS_CTRL [
        EXT_CFG OFFSET(0) NUMBITS(1) []
    ],
    /// F0x3F8 (DF3 DfGlobalCtrl). The PIE RAS step writes these fields; their
    /// names are not public (SOC_DF §3.6, §8).
    pub GLOBAL_CTRL [
        BIT3 OFFSET(3) NUMBITS(1) [],
        RAS_A OFFSET(4) NUMBITS(2) [],
        RAS_B0 OFFSET(8) NUMBITS(3) [],
        RAS_B1 OFFSET(12) NUMBITS(3) [],
        RAS_B2 OFFSET(16) NUMBITS(3) []
    ]
];

/// Registers whose layout DF2 and DF3 share.
pub mod regs {
    use super::*;

    pub const FBICNT: Reg<super::FBICNT::Register> = Reg::new(0, 0x40);
    pub const VGA_EN_OFFSET: u16 = 0x80;
    pub const CFG_ADDR_CTL: Reg<super::CFG_ADDR_CTL::Register> = Reg::new(0, 0x84);
    pub const CFG_MAP_OFFSET: u16 = 0xA0;
    pub const IO_BASE: Reg<super::IO_BASE::Register> = Reg::new(0, 0xC0);
    pub const IO_LIMIT_OFFSET: u16 = 0xC4;
    pub const DRAM_HOLE: Reg<super::DRAM_HOLE::Register> = Reg::new(0, 0x104);
    pub const DRAM_BASE_OFFSET: u16 = 0x110;
    pub const DRAM_LIMIT_OFFSET: u16 = 0x114;
    /// F0x200+16i MmioBaseAddress: address\[47:16\].
    pub const MMIO_BASE: Reg = Reg::new(0, 0x200);
    /// F0x204+16i MmioLimitAddress: address\[47:16\], inclusive.
    pub const MMIO_LIMIT: Reg = Reg::new(0, 0x204);
    pub const MMIO_CTRL_OFFSET: u16 = 0x208;
    /// F0x3F0: clock gating (written whole; SOC_DF §3.6).
    pub const CLOCK_GATING: Reg = Reg::new(0, 0x3F0);
    pub const GLOBAL_CTRL: Reg<super::GLOBAL_CTRL::Register> = Reg::new(0, 0x3F8);
    pub const SYSTEM_CFG_OFFSET: u16 = 0x200;
    pub const COMPONENT_CNT: Reg<super::COMPONENT_CNT::Register> = Reg::new(1, 0x204);
    pub const CFG_ACCESS_CTRL: Reg<super::CFG_ACCESS_CTRL::Register> = Reg::new(4, 0x44);
    pub const FICAA: u16 = 0x5C;
    pub const FICAD_LO: u16 = 0x98;
    /// F6x1F0/F6x1F4: PIE RAS status, cleared at init (SOC_DF §3.6).
    pub const PIE_RAS_STATUS_LO: Reg = Reg::new(6, 0x1F0);
    pub const PIE_RAS_STATUS_HI: Reg = Reg::new(6, 0x1F4);

    /// Strides of the register arrays.
    pub const MMIO_STRIDE: u16 = 0x10;
    pub const IO_STRIDE: u16 = 8;
    pub const CFG_MAP_STRIDE: u16 = 4;
    pub const DRAM_STRIDE: u16 = 8;
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

    fn mmio_ctrl_reg(pair: u8) -> Reg<Self::MmioCtrl> {
        Reg::new(0, regs::MMIO_CTRL_OFFSET).at(pair, regs::MMIO_STRIDE)
    }
    fn io_limit_reg(pair: u8) -> Reg<Self::IoLimit> {
        Reg::new(0, regs::IO_LIMIT_OFFSET).at(pair, regs::IO_STRIDE)
    }
    fn cfg_map_reg(map: u8) -> Reg<Self::CfgMap> {
        Reg::new(0, regs::CFG_MAP_OFFSET).at(map, regs::CFG_MAP_STRIDE)
    }
    fn vga_reg() -> Reg<Self::VgaEn> {
        Reg::new(0, regs::VGA_EN_OFFSET)
    }
}

/// The DF of a system, reached through config space on bus 0.
#[derive(Clone, Copy)]
pub struct Df<P> {
    pci: P,
}

impl<P: ConfigRegionAccess + Clone> Df<P> {
    pub const fn new(pci: P) -> Self {
        Self { pci }
    }

    /// The config access the DF uses (also reaches the host bridges' SMN).
    pub fn pci(&self) -> &P {
        &self.pci
    }

    /// Bus 0, device `0x18 + node`, function `func`.
    pub fn address(node: u8, func: u8) -> PciAddress {
        PciAddress::new(0, 0, NODE0_DEVICE + node, func)
    }

    /// `reg` of `node` at `target`.
    ///
    /// # Safety
    /// `node` exists, `reg` is a DF register valid for `target`, and the
    /// caller owns the node's FICAA/FICAD pair while instance accesses run
    /// (the pair is shared state, as SMN's index/data).
    pub unsafe fn reg<R: RegisterLongName>(
        &self,
        node: u8,
        target: Target,
        reg: Reg<R>,
    ) -> DfRegister<P, R> {
        debug_assert!(node < MAX_NODES && reg.offset & 3 == 0 && reg.offset <= 0x7FC);
        DfRegister {
            pci: self.pci.clone(),
            node,
            target,
            func: reg.func,
            offset: reg.offset,
            _reg: PhantomData,
        }
    }

    /// The node's own copy of `reg` ([`Target::Node`]).
    ///
    /// # Safety
    /// As [`reg`](Self::reg).
    pub unsafe fn node<R: RegisterLongName>(&self, node: u8, reg: Reg<R>) -> DfRegister<P, R> {
        // SAFETY: forwarded contract.
        unsafe { self.reg(node, Target::Node, reg) }
    }

    /// Instance `inst` of `reg` on `node` ([`Target::Instance`]).
    ///
    /// # Safety
    /// As [`reg`](Self::reg).
    pub unsafe fn inst<R: RegisterLongName>(
        &self,
        node: u8,
        inst: u8,
        reg: Reg<R>,
    ) -> DfRegister<P, R> {
        // SAFETY: forwarded contract.
        unsafe { self.reg(node, Target::Instance(inst), reg) }
    }
}

/// A DF register with the bitfields `R` on one node or fabric instance;
/// implements the tock-registers interfaces.
pub struct DfRegister<P, R: RegisterLongName = ()> {
    pci: P,
    node: u8,
    target: Target,
    func: u8,
    offset: u16,
    _reg: PhantomData<R>,
}

impl<P: ConfigRegionAccess + Clone, R: RegisterLongName> DfRegister<P, R> {
    fn config<S: RegisterLongName>(&self, func: u8, offset: u16) -> ConfigRegister<P, S> {
        // SAFETY: the constructor's contract covers the node's registers and
        // its FICAA/FICAD pair.
        unsafe { ConfigRegister::new(self.pci.clone(), Df::<P>::address(self.node, func), offset) }
    }

    /// The data register for this access; programs FICAA first for an
    /// instance access.
    fn data(&self) -> ConfigRegister<P, R> {
        match self.target {
            Target::Node => self.config(self.func, self.offset),
            Target::Instance(inst) => {
                self.config::<FICAA::Register>(4, regs::FICAA).write(
                    FICAA::TARGET_INST::SET
                        + FICAA::REG.val(u32::from(self.offset >> 2))
                        + FICAA::FUNC.val(u32::from(self.func))
                        + FICAA::INST.val(u32::from(inst)),
                );
                self.config(4, regs::FICAD_LO)
            }
        }
    }
}

impl<P: ConfigRegionAccess + Clone, R: RegisterLongName> Readable for DfRegister<P, R> {
    type T = u32;
    type R = R;

    fn get(&self) -> u32 {
        self.data().get()
    }
}

impl<P: ConfigRegionAccess + Clone, R: RegisterLongName> Writeable for DfRegister<P, R> {
    type T = u32;
    type R = R;

    fn set(&self, value: u32) {
        self.data().set(value)
    }
}

/// OR `bits` into a register without named fields.
#[cfg(any(feature = "naples", feature = "rome", feature = "milan"))]
pub(crate) fn or_bits<P: ConfigRegionAccess + Clone>(reg: &DfRegister<P>, bits: u32) {
    reg.set(reg.get() | bits)
}

#[cfg(test)]
mod tests {
    use super::model::DfModel;
    use super::*;

    #[test]
    fn node_and_instance_access() {
        let m = DfModel::default();
        let df = Df::new(m.clone());
        unsafe {
            df.node(1, regs::MMIO_BASE).set(0x1234);
            df.inst(1, 0x1B, regs::CFG_ADDR_CTL)
                .write(CFG_ADDR_CTL::BUS_NUM.val(0x40));
        }
        assert_eq!(m.get(1, None, 0, 0x200), 0x1234);
        assert_eq!(m.get(1, Some(0x1B), 0, 0x84), 0x40);
        // FICAA on the target node: inst 0x1B, F0, reg 0x84, instance bit.
        assert_eq!(m.ficaa(1), 0x1B << 16 | 0x84 | 1);
        let bus = unsafe { df.inst(1, 0x1B, regs::CFG_ADDR_CTL) }.read(CFG_ADDR_CTL::BUS_NUM);
        assert_eq!(bus, 0x40);
    }
}
