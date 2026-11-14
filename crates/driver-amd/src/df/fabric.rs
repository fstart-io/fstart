// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Data Fabric of a whole system: every node's registers by node
//! index, for the multi-socket SoCs (SOC_DF §3.1). A single node is
//! [`super::Df`].
use super::{DfReg, DfRegister, Target};
use fstart_pci::ConfigRegionAccess;
use tock_registers::RegisterLongName;
use tock_registers::interfaces::{Readable, Writeable};

/// The DF of a system, reached through config space on bus 0.
#[derive(Clone, Copy)]
pub struct Fabric<P> {
    pci: P,
}

impl<P: ConfigRegionAccess + Clone> Fabric<P> {
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
        nodes: &[super::topology::Node],
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
pub fn or_bits<P: ConfigRegionAccess + Clone>(reg: &DfRegister<P>, bits: u32) {
    reg.set(reg.get() | bits)
}

#[cfg(test)]
mod tests {
    use super::super::model::DfModel;
    use super::*;
    use crate::df::regs;

    #[test]
    fn node_and_instance_access() {
        let m = DfModel::default();
        let df = Fabric::new(m.clone());
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
