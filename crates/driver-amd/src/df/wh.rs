// SPDX-License-Identifier: MIT OR Apache-2.0
//! DFv4.5 (WH dual-IOD Data Fabric): builds on [`super::v4::Df`] (reusing its
//! FICAA/FICAD indirect access, CfgAddressMap, I/O, 56-bit MMIO and VGA
//! primitives), adding dual-IOD PCI node addressing (`0x18 + (socket << 1) + die`),
//! up to 16 config bus pairs, and DFv4.5 56-bit unshifted DRAM rules
//! (`F7x200..F7x5F0`, 32 slots, stride `0x20`, instances `0x24`/`0x25`).

use fstart_pci::{ConfigRegionAccess, PciAddress};
use tock_registers::interfaces::{Readable, Writeable};
use tock_registers::register_bitfields;

use super::v4;
use super::{BusRange, DfReg, DramRule};

/// Maximum DF nodes (2 sockets × 2 IODs: devices `0x18..=0x1B`).
pub const MAX_NODES: u8 = 4;
/// Maximum CfgAddressMap pairs on DFv4.5 (16 root bridges across 2P dual-IOD).
pub const CONFIG_PAIRS: u8 = 16;
/// Number of DFv4.5 DRAM address rule slots (`F7x200..F7x5F0`).
pub const DRAM_RULES: usize = 32;
/// Stride in bytes between consecutive DFv4.5 DRAM rule slots.
pub const DRAM_RULE_STRIDE: u16 = 0x20;
/// Base register offset of DFv4.5 DRAM rule 0 (`F7x200`).
pub const DRAM_RULE_BASE: u16 = 0x200;
/// Primary CCM/CS instances queried for DFv4.5 DRAM rules.
pub const DRAM_INSTANCE_PRIMARY: u8 = 0x24;
pub const DRAM_INSTANCE_SECONDARY: u8 = 0x25;

register_bitfields![u32,
    /// F7x204 / F7x20C + 0x20*i (32-bit): upper 24 bits (`[55:32]`) of a
    /// DFv4.5 unshifted 56-bit DRAM base or inclusive limit address.
    pub DramAddressHi [
        ADDRESS OFFSET(0) NUMBITS(24) []
    ],
    /// F7x210 + 0x20*i (32-bit): DFv4.5 DramAddressCtl (bit 0 = AddrRngVal,
    /// bits `[23:16]` = 8-bit DstFabricID).
    pub DramRuleCtl [
        VALID OFFSET(0) NUMBITS(1) [],
        DESTINATION OFFSET(16) NUMBITS(8) []
    ]
];

/// PCI device number on bus 0 for `(socket, die)`: `0x18 + (socket << 1) + die`.
pub const fn pci_device(socket: u8, die: u8) -> Option<u8> {
    if socket >= 2 || die >= 2 {
        return None;
    }
    Some(0x18 + (socket << 1) + die)
}

/// Linear DF node index `(socket << 1) | die` (`0..4`).
pub const fn node_index(socket: u8, die: u8) -> Option<u8> {
    if socket >= 2 || die >= 2 {
        return None;
    }
    Some((socket << 1) | die)
}

/// PCI configuration address for DF node `(socket, die)` function `func`.
pub fn node_address(socket: u8, die: u8, func: u8) -> Option<PciAddress> {
    let dev = pci_device(socket, die)?;
    if func >= 8 {
        return None;
    }
    Some(PciAddress::new(0, 0, dev, func))
}

/// One DFv4.5 node (one IOD) accessed through PCI configuration space.
///
/// Intentionally neither `Copy` nor `Clone` so exclusive FICAA/FICAD ownership
/// cannot be aliased.
pub struct Node<P> {
    df: v4::Df<P>,
    socket: u8,
    die: u8,
}

impl<P: ConfigRegionAccess> Node<P> {
    /// Construct a handle for `(socket, die)` (`socket < 2`, `die < 2`).
    ///
    /// # Safety
    /// Caller owns exclusive DF indirect register (FICAA/FICAD) access for this node.
    pub unsafe fn new(pci: P, socket: u8, die: u8) -> Option<Self> {
        let node = node_index(socket, die)?;
        Some(Self {
            // SAFETY: caller owns exclusive FICAA/FICAD access on `node`.
            df: unsafe { v4::Df::new(pci, node) },
            socket,
            die,
        })
    }

    pub const fn socket(&self) -> u8 {
        self.socket
    }

    pub const fn die(&self) -> u8 {
        self.die
    }

    pub const fn device(&self) -> u8 {
        0x18 + (self.socket << 1) + self.die
    }

    /// Underlying DFv4 register handle for shared `v4::program` / `v4::read` operations.
    pub const fn df(&self) -> &v4::Df<P> {
        &self.df
    }

    /// Direct read of a DF register on this node.
    pub fn read<R: tock_registers::RegisterLongName>(&self, reg: DfReg<R>) -> u32 {
        self.df.reg(reg).get()
    }

    /// Direct write of a DF register on this node.
    pub fn write<R: tock_registers::RegisterLongName>(&self, reg: DfReg<R>, value: u32) {
        self.df.reg(reg).set(value);
    }

    /// Instance-targeted read via DF4 FICAA (`F4x08C`) / FICAD_LO (`F4x0B8`).
    pub fn read_instance(&self, func: u8, offset: u16, instance: u8) -> u32 {
        self.df
            .instance(DfReg::<()>::new(func, offset), instance)
            .get()
    }

    /// Read DFv4.5 56-bit unshifted DRAM rule `slot` (`0..32`) from `instance`.
    pub fn dram_rule(&self, instance: u8, slot: usize) -> Option<DramRule> {
        if slot >= DRAM_RULES {
            return None;
        }
        let base_off = DRAM_RULE_BASE + (slot as u16) * DRAM_RULE_STRIDE;
        let ctl = self.read_instance(7, base_off + 0x10, instance);
        if !DramRuleCtl::VALID.is_set(ctl) {
            return None;
        }
        let base_lo = u64::from(self.read_instance(7, base_off, instance));
        let base_hi = u64::from(DramAddressHi::ADDRESS.read(self.read_instance(
            7,
            base_off + 0x04,
            instance,
        )));
        let limit_lo = u64::from(self.read_instance(7, base_off + 0x08, instance));
        let limit_hi = u64::from(DramAddressHi::ADDRESS.read(self.read_instance(
            7,
            base_off + 0x0c,
            instance,
        )));
        let base = base_lo | (base_hi << 32);
        let limit_inclusive = limit_lo | (limit_hi << 32);
        let limit = limit_inclusive.checked_add(1)?;
        if limit <= base {
            return None;
        }
        Some(DramRule {
            base,
            end: limit,
            dst: DramRuleCtl::DESTINATION.read(ctl) as u16,
            hole: false,
            socket_interleave: false,
            die_interleave: 0,
            channel_interleave: 0,
        })
    }

    /// Decode all valid DFv4.5 CfgAddressMap pairs (`0..16`) on this node.
    pub fn bus_routes(
        &self,
    ) -> Result<heapless::Vec<v4::BusRoute, { CONFIG_PAIRS as usize }>, v4::Error> {
        v4::bus_routes_n::<{ CONFIG_PAIRS as usize }, _>(&self.df)
    }

    /// Program one CfgAddressMap pair `index` (`0..16`) for `buses` -> 8-bit `destination`.
    pub fn set_config_route(&self, index: u8, buses: BusRange, destination: u16) -> Option<()> {
        if index >= CONFIG_PAIRS || buses.base > buses.limit || destination > 0xff {
            return None;
        }
        let base = v4::CONFIG_BASE.at(index, v4::CONFIG_STRIDE);
        let limit = v4::CONFIG_LIMIT.at(index, v4::CONFIG_STRIDE);
        self.df.reg(base).write(
            v4::ConfigBase::READ_ENABLE::SET
                + v4::ConfigBase::WRITE_ENABLE::SET
                + v4::ConfigBase::SEGMENT.val(0)
                + v4::ConfigBase::BUS_BASE.val(u32::from(buses.base)),
        );
        self.df.reg(limit).write(
            v4::ConfigLimit::DESTINATION.val(u32::from(destination))
                + v4::ConfigLimit::BUS_LIMIT.val(u32::from(buses.limit)),
        );
        Some(())
    }

    /// Route legacy VGA ranges to `destination` (8-bit WH fabric ID).
    pub fn route_legacy_vga(&self, destination: u16) {
        self.df
            .reg(v4::VGA)
            .write(v4::Vga::ENABLE::SET + v4::Vga::DESTINATION.val(u32::from(destination & 0xff)));
    }
}

/// Broadcast a CfgAddressMap pair across all populated dual-IOD `nodes`.
pub fn broadcast_config_route<P: ConfigRegionAccess>(
    nodes: &[Node<P>],
    index: u8,
    buses: BusRange,
    destination: u16,
) -> Option<()> {
    if index >= CONFIG_PAIRS || buses.base > buses.limit || destination > 0xff {
        return None;
    }
    for node in nodes {
        node.set_config_route(index, buses, destination)?;
    }
    Some(())
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct MockPci {
        regs: RefCell<BTreeMap<(u8, u8, u16), u32>>,
        indirect: RefCell<BTreeMap<(u8, u8, u8, u16), u32>>,
        last_ficaa: RefCell<BTreeMap<u8, u32>>,
    }

    impl ConfigRegionAccess for &MockPci {
        unsafe fn read(&self, address: PciAddress, offset: u16) -> u32 {
            let dev = address.device();
            let func = address.function();
            if func == 4 && offset == v4::FICAD_LO {
                let ficaa = *self.last_ficaa.borrow().get(&dev).unwrap_or(&0);
                let f = v4::Ficaa::FUNCTION.read(ficaa) as u8;
                let r = (v4::Ficaa::REGISTER.read(ficaa) as u16) << 2;
                let inst = v4::Ficaa::INSTANCE.read(ficaa) as u8;
                return *self.indirect.borrow().get(&(dev, inst, f, r)).unwrap_or(&0);
            }
            *self.regs.borrow().get(&(dev, func, offset)).unwrap_or(&0)
        }

        unsafe fn write(&self, address: PciAddress, offset: u16, value: u32) {
            let dev = address.device();
            let func = address.function();
            if func == 4 && offset == v4::FICAA {
                self.last_ficaa.borrow_mut().insert(dev, value);
                return;
            }
            self.regs.borrow_mut().insert((dev, func, offset), value);
        }
    }

    #[test]
    fn pci_devices_map_dual_iod_sockets() {
        assert_eq!(pci_device(0, 0), Some(0x18));
        assert_eq!(pci_device(0, 1), Some(0x19));
        assert_eq!(pci_device(1, 0), Some(0x1a));
        assert_eq!(pci_device(1, 1), Some(0x1b));
        assert_eq!(pci_device(2, 0), None);
        assert_eq!(pci_device(0, 2), None);
    }

    #[test]
    fn decodes_56bit_unshifted_dram_rule_and_masks_reserved_bits() {
        let pci = MockPci::default();
        let slot = 3usize;
        let off = DRAM_RULE_BASE + (slot as u16) * DRAM_RULE_STRIDE;
        let base: u64 = 0x0001_0000_0000;
        let limit_inc: u64 = 0x0002_ffff_ffff;
        let mut ind = pci.indirect.borrow_mut();
        ind.insert((0x19, DRAM_INSTANCE_PRIMARY, 7, off), base as u32);
        // Set upper reserved bits [31:24] to verify masking.
        ind.insert(
            (0x19, DRAM_INSTANCE_PRIMARY, 7, off + 4),
            0xff00_0000 | (base >> 32) as u32,
        );
        ind.insert((0x19, DRAM_INSTANCE_PRIMARY, 7, off + 8), limit_inc as u32);
        ind.insert(
            (0x19, DRAM_INSTANCE_PRIMARY, 7, off + 0x0c),
            0xaa00_0000 | (limit_inc >> 32) as u32,
        );
        // Set reserved bits [31:24] in ctl to verify 8-bit DstFabricID extraction.
        ind.insert(
            (0x19, DRAM_INSTANCE_PRIMARY, 7, off + 0x10),
            0xff00_0000 | (DramRuleCtl::VALID::SET + DramRuleCtl::DESTINATION.val(0x25)).value,
        );
        drop(ind);

        let node = unsafe { Node::new(&pci, 0, 1) }.unwrap();
        let rule = node.dram_rule(DRAM_INSTANCE_PRIMARY, slot).unwrap();
        assert_eq!(rule.base, 0x0001_0000_0000);
        assert_eq!(rule.end, 0x0003_0000_0000);
        assert!(!rule.hole);
        assert_eq!(rule.dst, 0x25);
    }

    #[test]
    fn broadcasts_and_reads_back_pair_15_across_nodes() {
        let pci = MockPci::default();
        let nodes = [
            unsafe { Node::new(&pci, 0, 0) }.unwrap(),
            unsafe { Node::new(&pci, 0, 1) }.unwrap(),
        ];
        let range = BusRange {
            base: 0xe0,
            limit: 0xff,
        };
        broadcast_config_route(&nodes, 15, range, 0x43).unwrap();
        // Out-of-range pair, inverted buses, and >8-bit destination must fail.
        assert!(broadcast_config_route(&nodes, 16, BusRange { base: 0, limit: 1 }, 0).is_none());
        assert!(broadcast_config_route(&nodes, 0, BusRange { base: 5, limit: 4 }, 0).is_none());
        assert!(broadcast_config_route(&nodes, 0, range, 0x100).is_none());

        for node in &nodes {
            let routes = node.bus_routes().unwrap();
            assert_eq!(routes.len(), 1);
            assert_eq!(routes[0].buses, range);
            assert_eq!(routes[0].destination, 0x43);
        }
    }
}
