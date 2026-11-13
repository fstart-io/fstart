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
use tock_registers::interfaces::{Readable, Writeable};

pub mod dram;
pub mod program;
pub mod topology;
pub mod v3;

pub mod milan;
pub mod naples;
pub mod rome;

#[cfg(test)]
#[allow(dead_code)]
pub(crate) mod model;

pub use crate::df::{BusRange, DramRule, Version};
use crate::df::{DfReg, DfRegister, Target};

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

    /// Each of `nodes` as its own [`crate::df::Df`], for the writers
    /// shared with the single-socket SoCs.
    ///
    /// # Safety
    /// As [`reg`](Self::reg), for every node in `nodes`.
    pub unsafe fn each(
        &self,
        nodes: &[crate::sp3::topology::Node],
    ) -> heapless::Vec<crate::df::Df<P>, { crate::df::MAX_NODES as usize }> {
        nodes
            .iter()
            // SAFETY: forwarded contract.
            .map(|n| unsafe {
                crate::df::Df::for_node(self.pci.clone(), n.device - crate::df::NODE0_DEVICE)
            })
            .collect()
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
pub(crate) fn or_bits<P: ConfigRegionAccess + Clone>(reg: &DfRegister<P>, bits: u32) {
    reg.set(reg.get() | bits)
}

#[cfg(test)]
mod tests {
    use super::model::DfModel;
    use super::*;
    use crate::df::regs;

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
