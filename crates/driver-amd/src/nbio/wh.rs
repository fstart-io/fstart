// SPDX-License-Identifier: MIT OR Apache-2.0
//! WH single-bank NBIO (`0x13B0_0000 + rb * 0x10_0000`, `rb = 0..4` per IOD):
//! early bus/SB-location setup, IOHC bridge controls, device/function remap,
//! IOAPIC BAR enable-then-lock and ID, 24-bridge INTx routing, IOMMU BAR and
//! L1/L2 initialization (`rb 0..=1` only, 512 KiB), PCIe AER strap, and
//! root-port TxRequester programming.
//!
//! Verified against openSIL `NbioBaseInitWh.c`, `NbioIommu.c` (`0xfc5ba8`),
//! `MpioInitFlowWh.c`, and WH PCIe strap / bridge-visibility routines.

use fstart_pci::{ConfigRegionAccess, PciAddress};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::register_bitfields;

use super::ioapic::{self, BridgeRouting, Route};
use super::iohc::{self, IohcBridgeCntl, NB_BUS_NUM_CNTL, NbBusNumCntl};
use super::iommu::{self, CAPABILITY as IOMMU_CAPABILITY, CAPABILITY_ID, CapHeader, Iommu};
use crate::smn::{Smn, SmnAccess, SmnReg, SmnRegister};

/// Number of root bridges per WH IOD.
pub const ROOT_BRIDGES_PER_IOD: u8 = 4;
/// Number of root bridges per WH IOD that carry an IOMMU (`rb 0..=1`).
pub const IOMMU_ROOT_BRIDGES_PER_IOD: u8 = 2;
/// Number of PCIe logical bridge slots per WH IOHC.
pub const PCIE_BRIDGES_PER_RB: u8 = 24;
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
    ],
    /// PCIECORE::PCIE_STRAP_F0 (`0x1A38_02C0 + (core << 20)`).
    pub PcieStrapF0 [
        AER_EN OFFSET(5) NUMBITS(1) []
    ],
    /// PCIe port transmit requester ID (`0x1A34_0084 + (core << 22) + (port << 12)`).
    pub TxRequester [
        FUNCTION OFFSET(0) NUMBITS(3) [],
        DEVICE OFFSET(3) NUMBITS(5) [],
        BUS OFFSET(8) NUMBITS(8) []
    ]
];

/// SMN 0x13B1_02F0 (32-bit): IOAPIC base address low (enable then lock).
pub const IOAPIC_BAR_LO: SmnReg<IoapicBarLo::Register> = SmnReg::new(0x13b1_02f0);
/// SMN 0x13B1_02F4 (32-bit): IOAPIC base address high (`[51:32]`).
pub const IOAPIC_BAR_HI: SmnReg = SmnReg::new(0x13b1_02f4);
/// SMN 0x1430_003C (32-bit): IOAPIC routing base control (set to `3`).
pub const IOAPIC_ROUTING_BASE: SmnReg<IoapicRoutingBase::Register> = SmnReg::new(0x1430_003c);

/// IOHC::SB_LOCATION (`0x13B1_007C`), cleared on non-primary root bridges.
const IOHC_SB_LOCATION: SmnReg = SmnReg::new(0x13b1_007c);
/// IOMMUL1 / IOAGR SB_LOCATION (`0x1530_0024`), `0x40001` on primary, `0` elsewhere.
const IOMMU_L1_SB_LOCATION: SmnReg = SmnReg::new(0x1530_0024);
/// IOMMUL2 SB_LOCATION (`0x13F0_112C`), `0x40001` on primary, `0` elsewhere.
const IOMMU_L2_SB_LOCATION: SmnReg = SmnReg::new(0x13f0_112c);
/// Primary IOHC SB location encoding programmed by openSIL `NbioBaseInitWh.c`.
const PRIMARY_SB_LOCATION: u32 = 0x0004_0001;

/// WH IOMMU L1 base addresses per root bridge (shifted by `rb * RB_SMN_STRIDE`).
const WH_IOMMU_L1: [u32; 2] = [0x1530_0000, 0x1470_0000];

/// SMN `0x13B1_00B8 + 4 * slot`: bridge device/function remap register.
pub const fn bridge_devfn(slot: usize) -> SmnReg<BridgeDevFn::Register> {
    SmnReg::new(0x13b1_00b8 + 4 * slot as u32)
}

/// Board policy for WH NBIO / IOMMU initialization.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Policy {
    /// Enable IOMMU L1/L2 clock gating (off by default to match openSIL).
    pub clock_gating: bool,
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

    pub fn smn(&self) -> &Smn<P> {
        &self.smn
    }

    pub fn pci(&self) -> &P {
        &self.pci
    }

    pub fn has_iommu(&self) -> bool {
        self.rb < IOMMU_ROOT_BRIDGES_PER_IOD
    }

    /// Early NBIO configuration matching openSIL `NbioBaseConfigurationWh`:
    /// programs `NB_BUS_NUM_CNTL` (`bus` + `LAT_MODE`) and sets/clears
    /// `SB_LOCATION` across IOHC, IOMMU L1, and IOMMU L2 so unclaimed config
    /// transactions on non-primary root bridges complete cleanly instead of
    /// hanging on a nonexistent FCH link.
    pub fn early(&self, is_primary: bool) {
        self.reg(NB_BUS_NUM_CNTL)
            .modify(NbBusNumCntl::BUS.val(self.bus().into()) + NbBusNumCntl::LAT_MODE::SET);
        if is_primary {
            if self.has_iommu() {
                self.reg(IOMMU_L1_SB_LOCATION).set(PRIMARY_SB_LOCATION);
            }
            self.reg(IOMMU_L2_SB_LOCATION).set(PRIMARY_SB_LOCATION);
        } else {
            self.reg(IOHC_SB_LOCATION).set(0);
            if self.has_iommu() {
                self.reg(IOMMU_L1_SB_LOCATION).set(0);
            }
            self.reg(IOMMU_L2_SB_LOCATION).set(0);
        }
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

    /// Show or hide IOHC PCIe root-port bridge `bridge`, matching openSIL WH
    /// `0x13B3_1004 + bridge * 0x400` (sets/clears `BRIDGE_DIS | CFG_DIS` and
    /// always enables `CRS_ENABLE`).
    pub fn set_bridge_visible(&self, bridge: usize, visible: bool) {
        let reg = iohc::iohc_bridge_cntl(bridge);
        self.reg(reg).modify(if visible {
            IohcBridgeCntl::BRIDGE_DIS::CLEAR
                + IohcBridgeCntl::CFG_DIS::CLEAR
                + IohcBridgeCntl::CRS_ENABLE::SET
        } else {
            IohcBridgeCntl::BRIDGE_DIS::SET
                + IohcBridgeCntl::CFG_DIS::SET
                + IohcBridgeCntl::CRS_ENABLE::SET
        });
    }

    /// Enable AER capability (`PCIE_STRAP_F0.AER_EN`) on WH PCIe core `core`
    /// (`0x1A38_02C0 + (core << 20)`), matching openSIL `SilVenicePei` `0xf7bfb7`.
    pub fn enable_aer(&self, core: u8) {
        if core < 8 {
            let reg = SmnReg::<PcieStrapF0::Register>::new(0x1a38_02c0 + (u32::from(core) << 20));
            self.smn.reg(reg).modify(PcieStrapF0::AER_EN::SET);
        }
    }

    /// Program a root port's transmit requester ID (`bus`, `devfn`) at
    /// `0x1A34_0084 + (core << 22) + (port << 12)` so port MSIs carry the
    /// correct requester ID into the IOMMU.
    pub fn set_requester(&self, core: u8, port: u8, bus: u8, devfn: u8) {
        let reg = SmnReg::<TxRequester::Register>::new(
            0x1a34_0084 + (u32::from(core) << 22) + (u32::from(port) << 12),
        );
        self.reg(reg).modify(
            TxRequester::BUS.val(u32::from(bus))
                + TxRequester::DEVICE.val(u32::from(devfn >> 3))
                + TxRequester::FUNCTION.val(u32::from(devfn & 7)),
        );
    }

    fn iommu_cap_read(&self, offset: u16) -> u32 {
        let addr = PciAddress::new(0, self.bus(), 0, 2);
        // SAFETY: Bus:00.2 is this root bridge's IOMMU PCI function.
        unsafe { self.pci.read(addr, IOMMU_CAPABILITY + offset) }
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
            self.pci.write(addr, IOMMU_CAP_BASE_LO, bar as u32);
            self.pci.write(addr, IOMMU_CAP_BASE_LO, (bar as u32) | 1);
        }
        true
    }

    /// Verify the IOMMU PCI capability header (`ID == 0x0F`) on `Bus:00.2` and
    /// decode the 512 KiB IOMMU MMIO BAR at `bar`.
    pub fn map_iommu(&self, bar: u32) -> bool {
        if !self.has_iommu() {
            return false;
        }
        let cap = tock_registers::LocalRegisterCopy::<u32, CapHeader::Register>::new(
            self.iommu_cap_read(0),
        );
        if cap.read(CapHeader::ID) != CAPABILITY_ID {
            return false;
        }
        self.set_iommu_bar(u64::from(bar))
    }

    /// Initialize WH IOMMU L1 and L2 registers from the openSIL `NbioIommu.c`
    /// (`0xfc5ba8`) policy table. Translation stays disabled until OS handoff.
    pub fn iommu_init(&self, _policy: Policy) {
        if !self.has_iommu() {
            return;
        }
        let rmw = |addr: u32, mask: u32, val: u32| {
            let r = self.reg(SmnReg::<()>::new(addr));
            r.set((r.get() & !mask) | (val & mask));
        };

        // IOMMU interrupt pin 0, clear HT_ATS_RESERVED, enable L2 misc bits.
        rmw(0x13f0_007c, 0x0f00_0007, 0x0800_0000);
        rmw(0x13f0_0050, 0x0040_0000, 0x0000_0000);
        rmw(0x0240_001c, 0x1400_0000, 0x1400_0000);

        // L1 units (IOAGR 0x1530_0000 and PCIe 0x1470_0000).
        for base in WH_IOMMU_L1 {
            rmw(base + 0x00c8, 0x8000_0005, 0x0000_0005);
            rmw(base + 0x001c, 0x0000_1040, 0x0000_1040);
            if base == 0x1470_0000 {
                rmw(base + 0x001c, 0x0001_0000, 0x0001_0000);
            }
            rmw(base + 0x009c, 0x0000_0006, 0x0000_0006);
            rmw(base + 0x0038, 0x1f00_0000, 0x1800_0000);
            rmw(base + 0x00d4, 0x0000_0001, 0x0000_0001);
            // Credit relocation window.
            rmw(base + 0x010c, 0x8000_0000, 0x8000_0000);
            rmw(base + 0x4018, 0x0000_00e0, 0x0000_00c0);
            self.reg(SmnReg::<()>::new(base + 0x02d8)).set(0x0001_ffff);
            rmw(base + 0x001c, 0x0000_0001, 0x0000_0001);
            rmw(base + 0x010c, 0x8000_0000, 0x0000_0000);
            // Hysteresis.
            rmw(base + 0x00cc, 0x007f_8000, 0x0010_0000);
        }
        // Enable x2APIC / XT support on L1.
        rmw(0x1470_00dc, 0x0000_0020, 0x0000_0020);

        // L2 and L2A policy registers.
        rmw(0x13f0_1200, 0x0000_0001, 0x0000_0001);
        rmw(0x1570_00c0, 0x0000_0001, 0x0000_0001);
        rmw(0x1570_00d0, 0x0000_7f7f, 0x0000_4949);
        rmw(0x13f0_1250, 0x0000_7f7f, 0x0000_4949);
        rmw(0x13f0_1130, 0x0007_f000, 0x0004_9000);
        rmw(0x13f0_111c, 0x0000_0005, 0x0000_0005);
        rmw(0x13f0_1158, 0x0000_0006, 0x0000_0002);
        rmw(0x13f0_114c, 0x0000_0001, 0x0000_0000);
        rmw(0x1570_0088, 0x0000_0001, 0x0000_0000);
        rmw(0x13f0_0084, 0x0008_0000, 0x0000_0000);
        rmw(0x13f0_00e8, 0x0000_3004, 0x0000_3004);
        rmw(0x1570_4398, 0x0000_2000, 0x0000_2000);
        rmw(0x13f0_0080, 0x0002_0000, 0x0002_0000);
        rmw(0x13f0_1240, 0x0000_0004, 0x0000_0004);
    }

    /// Capture IOMMU descriptors from PCI capability + MMIO EFR for ACPI IVRS.
    ///
    /// # Safety
    /// [`Self::map_iommu`] decoded the IOMMU at `base`, identity-mapped uncached.
    pub unsafe fn iommu(&self, base: u32) -> Option<Iommu> {
        if !self.has_iommu() {
            return None;
        }
        // SAFETY: caller's contract.
        unsafe {
            iommu::capture(
                u64::from(base),
                self.iommu_cap_read(0),
                self.iommu_cap_read(0x0c),
                self.iommu_cap_read(0x10),
            )
        }
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
    fn early_bus_sb_location_and_bridge_visibility_match_opensil_wh() {
        let space = SmnSpace::default();
        let rb0 = unsafe { RootBridge::new(&space, 0, 0x00) }.unwrap();
        let rb1 = unsafe { RootBridge::new(&space, 1, 0) }.unwrap();
        let rb2 = unsafe { RootBridge::new(&space, 2, 0) }.unwrap();

        rb0.early(true);
        assert_eq!(space.get(NB_BUS_NUM_CNTL.address()), 0x100);
        assert_eq!(
            space.get(IOMMU_L1_SB_LOCATION.address()),
            PRIMARY_SB_LOCATION
        );
        assert_eq!(
            space.get(IOMMU_L2_SB_LOCATION.address()),
            PRIMARY_SB_LOCATION
        );

        rb1.early(false);
        let s1 = RB_SMN_STRIDE;
        assert_eq!(space.get(NB_BUS_NUM_CNTL.address() + s1), 0x100);
        assert_eq!(space.get(IOHC_SB_LOCATION.address() + s1), 0);
        assert_eq!(space.get(IOMMU_L1_SB_LOCATION.address() + s1), 0);
        assert_eq!(space.get(IOMMU_L2_SB_LOCATION.address() + s1), 0);

        rb2.set_bridge_visible(3, false);
        let ctrl = iohc::iohc_bridge_cntl(3).address() + 2 * RB_SMN_STRIDE;
        assert_eq!(space.get(ctrl), 0x0004_0005);
        rb2.set_bridge_visible(3, true);
        assert_eq!(space.get(ctrl), 0x0004_0000);

        rb0.enable_aer(2);
        assert_eq!(space.get(0x1a38_02c0u32 + (2 << 20)), 1 << 5);
        rb1.set_requester(1, 3, 0x40, 0x19);
        assert_eq!(
            space.get(0x1a34_0084u32 + (1 << 22) + (3 << 12) + RB_SMN_STRIDE),
            (0x40 << 8) | 0x19
        );
    }

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
    fn iommu_only_exists_on_rb0_and_rb1_and_programs_l1_l2() {
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
        unsafe { pci.write(PciAddress::new(0, 0, 0, 2), 0x40, CAPABILITY_ID) };
        let rb0 = unsafe { RootBridge::new(&pci, 0, 0) }.unwrap();
        let rb1 = unsafe { RootBridge::new(&pci, 1, 0x40) }.unwrap();
        let rb2 = unsafe { RootBridge::new(&pci, 2, 0x80) }.unwrap();
        assert!(rb0.has_iommu() && rb1.has_iommu() && !rb2.has_iommu());
        assert!(!rb0.set_iommu_bar(0xfed8_1000)); // not 512 KiB aligned
        assert!(rb0.map_iommu(0xfed8_0000));
        assert_eq!(
            unsafe { pci.read(PciAddress::new(0, 0, 0, 2), IOMMU_CAP_BASE_LO) },
            0xfed8_0001
        );
        assert!(!rb1.map_iommu(0xfed8_0000)); // cap ID absent on rb1
        assert!(!rb2.set_iommu_bar(0xfed8_0000));
        assert!(unsafe { RootBridge::new(&pci, 4, 0) }.is_none());

        let space = SmnSpace::default();
        let rb1_smn = unsafe { RootBridge::new(&space, 1, 0) }.unwrap();
        rb1_smn.iommu_init(Policy::default());
        let s = RB_SMN_STRIDE;
        assert_eq!(space.get(0x13f0_1200 + s), 1);
        assert_eq!(space.get(0x13f0_1250 + s), 0x4949);
        assert_eq!(space.get(0x1470_00dc + s), 0x20);
        assert_eq!(space.get(0x1530_02d8 + s), 0x1_ffff);
    }
}
