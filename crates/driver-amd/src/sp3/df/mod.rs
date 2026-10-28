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

use fstart_pci::ConfigRegionAccess;
use tock_registers::RegisterLongName;
use tock_registers::fields::FieldValue;
use tock_registers::interfaces::{Readable, Writeable};

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

use crate::df::{DfReg, DfRegister, Target};

use crate::df::regs;

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
        reg: DfReg<R>,
    ) -> DfRegister<P, R> {
        // SAFETY: forwarded contract.
        unsafe { DfRegister::new(self.pci.clone(), node, target, reg) }
    }

    /// The node's own copy of `reg` ([`Target::Node`]).
    ///
    /// # Safety
    /// As [`reg`](Self::reg).
    pub unsafe fn node<R: RegisterLongName>(&self, node: u8, reg: DfReg<R>) -> DfRegister<P, R> {
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
        reg: DfReg<R>,
    ) -> DfRegister<P, R> {
        // SAFETY: forwarded contract.
        unsafe { self.reg(node, Target::Instance(inst), reg) }
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
                .write(regs::CFG_ADDR_CTL::BUS_NUM.val(0x40));
        }
        assert_eq!(m.get(1, None, 0, 0x200), 0x1234);
        assert_eq!(m.get(1, Some(0x1B), 0, 0x84), 0x40);
        // FICAA on the target node: inst 0x1B, F0, reg 0x84, instance bit.
        assert_eq!(m.ficaa(1), 0x1B << 16 | 0x84 | 1);
        let bus = unsafe { df.inst(1, 0x1B, regs::CFG_ADDR_CTL) }.read(regs::CFG_ADDR_CTL::BUS_NUM);
        assert_eq!(bus, 0x40);
    }
}
