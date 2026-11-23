// SPDX-License-Identifier: MIT OR Apache-2.0
//! WH single-bank NBIO (`0x13B0_0000 + rb * 0x10_0000`, `rb = 0..4` per IOD):
//! IOHC bridge controls, device/function remap, IOAPIC BAR enable-then-lock,
//! IOAPIC ID, IOMMU BAR (`rb 0..=1` only, 512 KiB), and the 24-bridge INTx
//! routing table verified from WH silicon init.

use fstart_pci::{ConfigRegionAccess, PciAddress};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::register_bitfields;

use super::ioapic::{self, BridgeRouting, Route};
use super::iohc::{self, IohcBridgeCntl};
use crate::smn::{Smn, SmnAccess, SmnReg, SmnRegister};

/// Number of root bridges per WH IOD.
pub const ROOT_BRIDGES_PER_IOD: u8 = 4;
/// Number of root bridges per WH IOD that carry an IOMMU (`rb 0..=1`).
pub const IOMMU_ROOT_BRIDGES_PER_IOD: u8 = 2;
/// IOMMU MMIO aperture size (512 KiB).
pub const IOMMU_BAR_SIZE: u64 = super::iommu::MMIO_SIZE;
/// Stride in SMN between consecutive root bridges on one WH IOD.
pub const RB_SMN_STRIDE: u32 = 0x10_0000;
/// PCI config offset of the IOMMU base address low dword on `Bus:00.2`.
pub const IOMMU_CAP_BASE_LO: u16 = 0x44;
pub const IOMMU_CAP_BASE_HI: u16 = 0x48;

register_bitfields![u32,
    /// SMN 0x13B1_02F0 (32-bit): IOHC::IOAPIC_BASE_ADDR_LO (enable bit 0, lock bit 1).
    pub IoapicBarLo [
        ENABLE OFFSET(0) NUMBITS(1) [],
        LOCK OFFSET(1) NUMBITS(1) [],
        ADDRESS OFFSET(8) NUMBITS(24) []
    ],
    /// SMN 0x13B1_00B8 + 4*slot (32-bit): IOHC::IOHC_Bridge_DevFn.
    pub BridgeDevFn [
        FUNCTION OFFSET(0) NUMBITS(3) [],
        DEVICE OFFSET(3) NUMBITS(5) []
    ],
    /// SMN 0x1430_003C (32-bit): IOAPIC bridge routing base / control.
    pub IoapicRoutingBase [
        BASE OFFSET(0) NUMBITS(5) []
    ]
];

/// SMN 0x13B1_02F0 (32-bit): IOAPIC base address low (enable then lock).
pub const IOAPIC_BAR_LO: SmnReg<IoapicBarLo::Register> = SmnReg::new(0x13b1_02f0);
/// SMN 0x13B1_02F4 (32-bit): IOAPIC base address high (`[51:32]`).
pub const IOAPIC_BAR_HI: SmnReg = SmnReg::new(0x13b1_02f4);
/// SMN 0x1430_003C (32-bit): IOAPIC routing base control (set to `3`).
pub const IOAPIC_ROUTING_BASE: SmnReg<IoapicRoutingBase::Register> = SmnReg::new(0x1430_003c);

/// SMN `0x13B1_00B8 + 4 * slot`: bridge device/function remap register.
pub const fn bridge_devfn(slot: usize) -> SmnReg<BridgeDevFn::Register> {
    SmnReg::new(0x13b1_00b8 + 4 * slot as u32)
}

/// 24-bridge INTx routing table into a WH IOHC IOAPIC (`0x1430_0040..=0x1430_009C`,
/// mask `0x001F_0037`: groups `0..=5`, swizzles `0..=3`, maps `0..=4`), extracted
/// verified from WH NBIO silicon init.
pub const ROUTES: [Route; 24] = {
    const TRIPLES: [(u8, u8, u8); 24] = [
        (0, 0, 0),
        (1, 0, 0),
        (2, 0, 0),
        (3, 0, 0),
        (4, 0, 0),
        (5, 0, 0),
        (5, 2, 0),
        (4, 2, 0),
        (3, 2, 1),
        (2, 2, 1),
        (1, 2, 1),
        (0, 2, 1),
        (0, 3, 1),
        (1, 3, 1),
        (2, 3, 1),
        (3, 3, 1),
        (4, 3, 2),
        (5, 3, 2),
        (5, 1, 2),
        (4, 1, 2),
        (3, 1, 3),
        (2, 1, 3),
        (1, 1, 4),
        (0, 1, 4),
    ];
    let mut routes = [Route {
        group: 0,
        swizzle: 0,
        map: 0,
    }; 24];
    let mut i = 0;
    while i < routes.len() {
        let (group, swizzle, map) = TRIPLES[i];
        routes[i] = Route {
            group,
            swizzle,
            map,
        };
        i += 1;
    }
    routes
};

/// One WH root bridge's NBIO (`rb = 0..4` on an IOD), shifting SMN registers
/// by `rb * 0x10_0000`.
#[derive(Clone)]
pub struct RootBridge<P> {
    smn: Smn<P>,
    pci: P,
    pub rb: u8,
}

impl<P: ConfigRegionAccess + Clone> RootBridge<P> {
    /// Construct a WH NBIO handle for root bridge `rb` (`0..4`) on `bus`.
    ///
    /// # Safety
    /// `bus` routes to this root bridge and caller owns its SMN pair.
    pub unsafe fn new(pci: P, rb: u8, bus: u8) -> Option<Self> {
        if rb >= ROOT_BRIDGES_PER_IOD {
            return None;
        }
        Some(Self {
            // SAFETY: caller's contract.
            smn: unsafe { Smn::on_bus(pci.clone(), bus) },
            pci,
            rb,
        })
    }

    pub fn bus(&self) -> u8 {
        self.smn.bus()
    }

    pub fn has_iommu(&self) -> bool {
        self.rb < IOMMU_ROOT_BRIDGES_PER_IOD
    }

    /// Program the IOHC IOAPIC BAR (`0x13B1_02F4` high, `0x13B1_02F0` low with
    /// enable bit 0, then lock bit 1) and assign the IOAPIC ID (`0x0280_1000`).
    pub fn init_ioapic(&self, bar: u64, id: u8) {
        self.reg(IOAPIC_BAR_HI)
            .set(((bar >> 32) as u32) & 0x000f_ffff);
        self.reg(IOAPIC_BAR_LO)
            .write(IoapicBarLo::ADDRESS.val((bar as u32) >> 8) + IoapicBarLo::ENABLE::SET);
        self.reg(IOAPIC_BAR_LO).modify(IoapicBarLo::LOCK::SET);
        self.reg(ioapic::IOAPIC_ID)
            .write(ioapic::IoapicId::ID.val(u32::from(id)));
    }

    /// Program all 24 bridge INTx routing registers (`0x1430_0040..=0x1430_009C`)
    /// and routing base control (`0x1430_003C = 3`).
    pub fn init_ioapic_routes(&self) {
        for (i, route) in ROUTES.iter().enumerate() {
            self.reg(ioapic::br_interrupt_routing(i)).modify(
                BridgeRouting::GROUP.val(u32::from(route.group))
                    + BridgeRouting::SWIZZLE.val(u32::from(route.swizzle))
                    + BridgeRouting::MAP.val(u32::from(route.map)),
            );
        }
        self.reg(IOAPIC_ROUTING_BASE)
            .modify(IoapicRoutingBase::BASE.val(3));
    }

    /// Read the active PCI `(device, function)` programmed for IOHC bridge `slot`.
    pub fn bridge_devfn(&self, slot: usize) -> (u8, u8) {
        let reg = self.reg(bridge_devfn(slot));
        (
            reg.read(BridgeDevFn::DEVICE) as u8,
            reg.read(BridgeDevFn::FUNCTION) as u8,
        )
    }

    /// Show or hide IOHC PCIe root-port bridge `bridge`.
    pub fn set_bridge_visible(&self, bridge: usize, visible: bool) {
        let reg = iohc::iohc_bridge_cntl(bridge);
        if visible {
            self.reg(reg).modify(IohcBridgeCntl::BRIDGE_DIS::CLEAR);
        } else {
            self.reg(reg).modify(IohcBridgeCntl::BRIDGE_DIS::SET);
        }
    }

    /// Program the IOMMU MMIO BAR on `Bus:00.2 + 0x44/0x48` (`rb 0..=1` only).
    /// Returns `false` if this root bridge has no IOMMU or `bar` is misaligned.
    pub fn set_iommu_bar(&self, bar: u64) -> bool {
        if !self.has_iommu() || (bar & (IOMMU_BAR_SIZE - 1)) != 0 {
            return false;
        }
        let addr = PciAddress::new(0, self.bus(), 0, 2);
        // SAFETY: Bus:00.2 is the root bridge's IOMMU PCI function.
        unsafe {
            self.pci.write(addr, IOMMU_CAP_BASE_HI, (bar >> 32) as u32);
            self.pci.write(addr, IOMMU_CAP_BASE_LO, (bar as u32) | 1);
        }
        true
    }
}

impl<P: ConfigRegionAccess + Clone> SmnAccess for RootBridge<P> {
    type Reg<'a, R: tock_registers::RegisterLongName>
        = SmnRegister<'a, P, R>
    where
        Self: 'a;

    fn reg<R: tock_registers::RegisterLongName>(&self, reg: SmnReg<R>) -> Self::Reg<'_, R> {
        self.smn.reg(reg.offset(u32::from(self.rb) * RB_SMN_STRIDE))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smn::testing::SmnSpace;

    #[test]
    fn ioapic_enable_lock_id_and_routes_use_rb_stride() {
        let space = SmnSpace::default();
        let nb = unsafe { RootBridge::new(&space, 2, 0) }.unwrap();
        assert!(!nb.has_iommu());
        nb.init_ioapic(0xfec0_1000, 0x12);
        let shift = 2 * RB_SMN_STRIDE;
        assert_eq!(space.get(IOAPIC_BAR_HI.address() + shift), 0);
        assert_eq!(space.get(IOAPIC_BAR_LO.address() + shift), 0xfec0_1003);
        assert_eq!(space.get(ioapic::IOAPIC_ID.address() + shift), 0x12 << 24);

        nb.init_ioapic_routes();
        assert_eq!(
            space.get(ioapic::br_interrupt_routing(0).address() + shift),
            0x0000_0000
        );
        assert_eq!(
            space.get(ioapic::br_interrupt_routing(6).address() + shift),
            0x0000_0025
        );
        assert_eq!(
            space.get(ioapic::br_interrupt_routing(15).address() + shift),
            0x0001_0033
        );
        assert_eq!(
            space.get(ioapic::br_interrupt_routing(23).address() + shift),
            0x0004_0010
        );
        assert_eq!(space.get(IOAPIC_ROUTING_BASE.address() + shift), 3);
    }

    #[test]
    fn iommu_only_exists_on_rb0_and_rb1() {
        use std::cell::RefCell;
        use std::collections::BTreeMap;

        #[derive(Default)]
        struct FakePci(RefCell<BTreeMap<(u8, u8, u8, u16), u32>>);

        impl ConfigRegionAccess for FakePci {
            unsafe fn read(&self, addr: PciAddress, offset: u16) -> u32 {
                *self
                    .0
                    .borrow()
                    .get(&(addr.bus(), addr.device(), addr.function(), offset))
                    .unwrap_or(&0)
            }
            unsafe fn write(&self, addr: PciAddress, offset: u16, value: u32) {
                self.0
                    .borrow_mut()
                    .insert((addr.bus(), addr.device(), addr.function(), offset), value);
            }
        }

        let pci = FakePci::default();
        let rb0 = unsafe { RootBridge::new(&pci, 0, 0) }.unwrap();
        let rb1 = unsafe { RootBridge::new(&pci, 1, 0x40) }.unwrap();
        let rb2 = unsafe { RootBridge::new(&pci, 2, 0x80) }.unwrap();
        assert!(rb0.has_iommu() && rb1.has_iommu() && !rb2.has_iommu());
        assert!(!rb0.set_iommu_bar(0xfed8_1000)); // not 512 KiB aligned
        assert!(rb0.set_iommu_bar(0x0001_fed8_0000));
        assert_eq!(
            unsafe { pci.read(PciAddress::new(0, 0, 0, 2), IOMMU_CAP_BASE_LO) },
            0xfed8_0001
        );
        assert_eq!(
            unsafe { pci.read(PciAddress::new(0, 0, 0, 2), IOMMU_CAP_BASE_HI) },
            1
        );
        assert!(!rb2.set_iommu_bar(0xfed8_0000));
        assert!(unsafe { RootBridge::new(&pci, 4, 0) }.is_none());
    }
}
