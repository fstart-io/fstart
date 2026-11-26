// SPDX-License-Identifier: MIT OR Apache-2.0
//! WH single-bank NBIO (`0x13B0_0000 + iohc * 0x10_0000`, `iohc` the IOHC
//! of a root bridge on its IOD, [`crate::dxio::wh::Layout::iohc`]): early bus/SB-location setup, the VDM node bus
//! ranges, IOHC bridge controls, device/function remap,
//! IOAPIC BAR enable-then-lock and ID, the IOMMU BAR (IOHCs 0 and 1 only, 512
//! KiB), and the vendor sequence with its generated tables ([`init`],
//! [`tables`]); the PCIe core/port registers MPIO reaches for the host are
//! in [`pcie`].
//!
//! Verified against openSIL `NbioBaseInitWh.c`, `NbioIommu.c` (`0xfc5ba8`),
//! `MpioInitFlowWh.c`, and WH PCIe strap / bridge-visibility routines.

use fstart_pci::{ConfigRegionAccess, PciAddress};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::register_bitfields;

use super::ioapic::{self, Route};
use super::iohc;
use super::iommu::{self, CAPABILITY as IOMMU_CAPABILITY, CAPABILITY_ID, CapHeader, Iommu};
use crate::smn::{Smn, SmnAccess, SmnReg, SmnRegister};

pub mod bars;
pub mod init;
pub mod pcie;
pub mod ras;
#[rustfmt::skip]
pub mod tables;

pub use init::{
    Fuses, Nbif, Part, PerfRecord, Policy, Ras, Record, base_properties, iommu_properties,
    perf_table, property, ras_properties,
};

/// IOHC SMN slots on a WH IOD (`iohc * 0x10_0000`), the bound of the IOHC
/// index and of a root bridge's index on its IOD. How many root bridges an
/// IOD has is the count of its IOS: 2 on SP7 A0, 3 on SP7 B0, 4 on
/// `0x00B9`/`0x00BA` ([`crate::cpu::wh::Part::root_bridges_per_iod`];
/// AmdFabricWhDxe `FUN_8d00`).
pub const MAX_ROOT_BRIDGES_PER_IOD: u8 = 4;
/// Number of root bridges per WH IOD that carry an IOMMU (IOHCs 0 and 1).
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

/// IOMMUL1 / IOAGR SB_LOCATION (`0x1530_0024`), `0x40001` on primary, `0` elsewhere.
const IOMMU_L1_SB_LOCATION: SmnReg = SmnReg::new(0x1530_0024);
/// IOMMUL2 SB_LOCATION (`0x13F0_112C`), `0x40001` on primary, `0` elsewhere.
const IOMMU_L2_SB_LOCATION: SmnReg = SmnReg::new(0x13f0_112c);
/// Primary IOHC SB location encoding programmed by openSIL `NbioBaseInitWh.c`.
const PRIMARY_SB_LOCATION: u32 = 0x0004_0001;

/// The VDM node bus range table of an IOHC (`InitVdmNodeBusRangesWh`,
/// `0xf32db0`): entry `e` at `0x13B1_0190 + e * stride` holds the buses of
/// the system's `e`-th root bridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VdmTable {
    pub entries: u8,
    pub stride: u32,
}

/// SP7 B0 and later: 16 entries, 8 bytes apart.
pub const VDM_TABLE: VdmTable = VdmTable {
    entries: 16,
    stride: 8,
};
/// SP7 A0 (`0x00B5_0F00`): 8 entries, 16 bytes apart.
pub const VDM_TABLE_A0: VdmTable = VdmTable {
    entries: 8,
    stride: 0x10,
};

/// VDM node bus range entry `e`.
pub const fn vdm_bus_range(table: VdmTable, e: u8) -> SmnReg<iohc::IohcRbBusRoute::Register> {
    SmnReg::new(0x13b1_0190 + e as u32 * table.stride)
}

/// The IOAPIC ID of the system's `index`-th root bridge: `base + index`
/// (`NbioIoApicPreDefIdWh`, `0xf32c40`); `None` past `0xFF`, where the
/// vendor stops.
pub const fn ioapic_id(base: u8, index: usize) -> Option<u8> {
    if index > (u8::MAX - base) as usize {
        None
    } else {
        Some(base + index as u8)
    }
}

/// 24-bridge INTx routing table into a WH IOHC IOAPIC (`0x1430_0040..=0x1430_009C`,
/// mask `0x001F_0037`: groups `0..=5`, swizzles `0..=3`, maps `0..=4`), as the
/// `_PRT` sees it. The vendor programs bridges 0..15 on every part (`BASE`)
/// and 16..23 on A0 only (`BASE_A0_IOAPIC_ROUTES`); B0 keeps their reset
/// values, which are taken to be these (NBIO.md Q3).
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

/// One WH root bridge's NBIO: its IOHC (`iohc = 0..4` on an IOD), shifting
/// SMN registers by `iohc * 0x10_0000`, and its root bus.
#[derive(Clone)]
pub struct RootBridge<P> {
    smn: Smn<P>,
    pci: P,
    /// The IOHC index ([`crate::dxio::wh::Layout::iohc`] of the physical
    /// root bridge).
    pub iohc: u8,
}

/// What [`RootBridge::numbering`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Numbering {
    /// The IOHC's `NB_BUS_NUM_CNTL` names the handle's root bus.
    Agrees,
    /// No IOHC of the IOD has a bus number yet: nothing to compare.
    Unprogrammed,
    /// The IOHC names another bus, or none while IOHC `found` names this
    /// one: the IOHC numbering ([`crate::dxio::wh::Layout::iohc`]) is not
    /// the fabric's.
    Disagrees { bus: Option<u8>, found: Option<u8> },
}

impl<P: ConfigRegionAccess + Clone> RootBridge<P> {
    /// Construct a WH NBIO handle for IOHC `iohc` of the root bridge on
    /// `bus`.
    ///
    /// # Safety
    /// `bus` routes to this root bridge and caller owns its SMN pair.
    pub unsafe fn new(pci: P, iohc: u8, bus: u8) -> Option<Self> {
        // SAFETY: caller's contract.
        unsafe { Self::on_root(pci, iohc, PciAddress::new(0, bus, 0, 0)) }
    }

    /// [`Self::new`] for the root bridge whose root bus is `root` (segment
    /// and bus) on a fabric with several PCI segments.
    ///
    /// # Safety
    /// As [`Self::new`].
    pub unsafe fn on_root(pci: P, iohc: u8, root: PciAddress) -> Option<Self> {
        if iohc >= MAX_ROOT_BRIDGES_PER_IOD {
            return None;
        }
        Some(Self {
            // SAFETY: caller's contract.
            smn: unsafe { Smn::on_root(pci.clone(), root) },
            pci,
            iohc,
        })
    }

    pub fn bus(&self) -> u8 {
        self.smn.bus()
    }

    /// The PCI segment of the root bus.
    pub fn segment(&self) -> u16 {
        self.smn.segment()
    }

    /// `function` of device 0 on the root bus (00.2 is the IOMMU).
    fn function(&self, function: u8) -> PciAddress {
        PciAddress::new(self.segment(), self.bus(), 0, function)
    }

    /// Whether this handle's IOHC is the one the DF routes its root bus to:
    /// its `NB_BUS_NUM_CNTL` (`0x13B1_0044 + iohc << 20`) as the firmware
    /// before us left it names the bus, or, when it does not, which other
    /// IOHC of the IOD does. Run before [`Self::early`] writes the bus. The
    /// third root bridge of SP7 B0 being IOHC 3 is decompiler-inferred
    /// (NBIO.md, `OverrideRbIndex`); every NBIO, MPIO and CXL step of that
    /// root bridge depends on it.
    pub fn numbering(&self) -> Numbering {
        let claims = |iohc: u8| {
            let reg = self
                .smn
                .reg(iohc::NB_BUS_NUM_CNTL.offset(u32::from(iohc) * RB_SMN_STRIDE))
                .extract();
            (reg.get() != u32::MAX && reg.is_set(iohc::NbBusNumCntl::LAT_MODE))
                .then(|| reg.read(iohc::NbBusNumCntl::BUS) as u8)
        };
        let own = claims(self.iohc);
        if own == Some(self.bus()) {
            return Numbering::Agrees;
        }
        let found = (0..MAX_ROOT_BRIDGES_PER_IOD)
            .filter(|&i| i != self.iohc)
            .find(|&i| claims(i) == Some(self.bus()));
        if own.is_none() && found.is_none() {
            Numbering::Unprogrammed
        } else {
            Numbering::Disagrees { bus: own, found }
        }
    }

    pub fn smn(&self) -> &Smn<P> {
        &self.smn
    }

    pub fn pci(&self) -> &P {
        &self.pci
    }

    /// IOHCs 0 and 1 carry an IOMMU.
    pub fn has_iommu(&self) -> bool {
        self.iohc < IOMMU_ROOT_BRIDGES_PER_IOD
    }

    /// Fill this IOHC's VDM node bus range table with the buses of every
    /// root bridge of the system (`ranges`, system order, at most
    /// `table.entries`), marking every entry but its own (`own`) as
    /// another node's (`InitVdmNodeBusRangesWh`, `0xf32db0`; Dell and
    /// GIGABYTE alike). Returns `false` if there are more root bridges than
    /// entries.
    pub fn set_vdm_bus_ranges(
        &self,
        table: VdmTable,
        ranges: &[crate::df::BusRange],
        own: usize,
    ) -> bool {
        if ranges.len() > usize::from(table.entries) {
            return false;
        }
        for (e, range) in ranges.iter().enumerate() {
            self.reg(vdm_bus_range(table, e as u8)).write(
                iohc::IohcRbBusRoute::BASE.val(range.base.into())
                    + iohc::IohcRbBusRoute::LIMIT.val(range.limit.into())
                    + iohc::IohcRbBusRoute::REMOTE.val(u32::from(e != own)),
            );
        }
        true
    }

    /// Program the IOHC IOAPIC BAR (`0x13B1_02F4` high, `0x13B1_02F0` low with
    /// enable bit 0, then lock bit 1), verify via readback, and assign the
    /// IOAPIC ID (`0x0280_1000`).
    ///
    /// Returns `false` if `bar` is zero or misaligned (256-byte boundary), if
    /// the SMN aperture reads all-ones, if the BAR is already locked to a
    /// different address or disabled, or if readback does not match.
    pub fn init_ioapic(&self, bar: u64, id: u8) -> bool {
        if bar == 0 || (bar & 0xff) != 0 || (bar >> 52) != 0 {
            return false;
        }
        let want_hi = ((bar >> 32) as u32) & 0x000f_ffff;
        let want_lo = IoapicBarLo::ADDRESS.val((bar as u32) >> 8)
            + IoapicBarLo::ENABLE::SET
            + IoapicBarLo::LOCK::SET;
        let lo_reg = self.reg(IOAPIC_BAR_LO);
        let hi_reg = self.reg(IOAPIC_BAR_HI);
        let cur_lo = lo_reg.get();
        let cur_hi = hi_reg.get();
        if cur_lo == u32::MAX || cur_hi == u32::MAX {
            return false;
        }
        if lo_reg.is_set(IoapicBarLo::LOCK) {
            if (cur_hi & 0x000f_ffff) != want_hi || !lo_reg.matches_all(want_lo) {
                return false;
            }
        } else {
            hi_reg.set(want_hi);
            lo_reg.write(IoapicBarLo::ADDRESS.val((bar as u32) >> 8) + IoapicBarLo::ENABLE::SET);
            lo_reg.modify(IoapicBarLo::LOCK::SET);
            let got_hi = hi_reg.get();
            let got_lo = lo_reg.get();
            if got_hi == u32::MAX
                || got_lo == u32::MAX
                || (got_hi & 0x000f_ffff) != want_hi
                || !lo_reg.matches_all(want_lo)
            {
                return false;
            }
        }
        let id_reg = self.reg(ioapic::IOAPIC_ID);
        id_reg.write(ioapic::IoapicId::ID.val(u32::from(id)));
        id_reg.get() != u32::MAX && id_reg.read(ioapic::IoapicId::ID) == u32::from(id)
    }

    /// Show or hide IOHC PCIe root-port bridge `bridge` (`0x13B3_1004 +
    /// bridge * 0x400`, `MpioControlPortsWh` `0xf66b80`): set or clear
    /// `BRIDGE_DIS | CFG_DIS`, clear the port descriptor bit 5 (fstart's
    /// ports have byte `0x7B` 0) and always enable `CRS_ENABLE`.
    pub fn set_bridge_visible(&self, bridge: usize, visible: bool) {
        use iohc::IohcBridgeCntl as Cntl;
        let shown = if visible {
            Cntl::BRIDGE_DIS::CLEAR + Cntl::CFG_DIS::CLEAR
        } else {
            Cntl::BRIDGE_DIS::SET + Cntl::CFG_DIS::SET
        };
        self.reg(iohc::iohc_bridge_cntl(bridge))
            .modify(shown + Cntl::PORT_7B::CLEAR + Cntl::CRS_ENABLE::SET);
    }

    fn iommu_cap_read(&self, offset: u16) -> u32 {
        let addr = self.function(2);
        // SAFETY: Bus:00.2 is this root bridge's IOMMU PCI function.
        unsafe { self.pci.read(addr, IOMMU_CAPABILITY + offset) }
    }

    /// Program the IOMMU MMIO BAR on `Bus:00.2 + 0x44/0x48` (IOHCs 0 and 1
    /// only), its decode still off, and verify via PCI config readback; the
    /// vendor turns the decode on at AmdNbioDxe's entry
    /// ([`Self::enable_iommu_mmio`]).
    /// Returns `false` if this root bridge has no IOMMU, `bar` is zero or
    /// misaligned, or readback is all-ones / does not match `bar`.
    pub fn set_iommu_bar(&self, bar: u64) -> bool {
        if !self.has_iommu() || bar == 0 || (bar & (IOMMU_BAR_SIZE - 1)) != 0 {
            return false;
        }
        let addr = self.function(2);
        let want_hi = (bar >> 32) as u32;
        let want_lo = bar as u32;
        // SAFETY: Bus:00.2 is the root bridge's IOMMU PCI function.
        unsafe {
            self.pci.write(addr, IOMMU_CAP_BASE_HI, want_hi);
            self.pci.write(addr, IOMMU_CAP_BASE_LO, want_lo);
            let got_hi = self.pci.read(addr, IOMMU_CAP_BASE_HI);
            let got_lo = self.pci.read(addr, IOMMU_CAP_BASE_LO);
            got_hi != u32::MAX && got_lo != u32::MAX && got_hi == want_hi && got_lo == want_lo
        }
    }

    /// Verify the IOMMU PCI capability header (`ID == 0x0F`) on `Bus:00.2` and
    /// decode the 512 KiB IOMMU MMIO BAR at `bar`.
    pub fn map_iommu(&self, bar: u32) -> bool {
        if !self.has_iommu() {
            return false;
        }
        let raw = self.iommu_cap_read(0);
        if raw == u32::MAX {
            return false;
        }
        let cap = tock_registers::LocalRegisterCopy::<u32, CapHeader::Register>::new(raw);
        if cap.read(CapHeader::ID) != CAPABILITY_ID {
            return false;
        }
        self.set_iommu_bar(u64::from(bar))
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
        self.smn
            .reg(reg.offset(u32::from(self.iohc) * RB_SMN_STRIDE))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smn::testing::SmnSpace;

    pub(super) const B0: Part = Part::from_cpuid(0x00b6_0f00, 2 << 28);
    pub(super) const A0: Part = Part::from_cpuid(0x00b5_0f00, 2 << 28);

    #[test]
    fn early_bus_sb_location_and_bridge_visibility_match_opensil_wh() {
        use super::iohc::NB_BUS_NUM_CNTL;
        let space = SmnSpace::default();
        let rb0 = unsafe { RootBridge::new(&space, 0, 0x00) }.unwrap();
        let rb1 = unsafe { RootBridge::new(&space, 1, 0) }.unwrap();
        let rb2 = unsafe { RootBridge::new(&space, 2, 0) }.unwrap();
        let policy = Policy::default();

        rb0.early(B0, &policy);
        assert_eq!(space.get(NB_BUS_NUM_CNTL.address()), 0x100);
        assert_eq!(space.get(iohc::SB_LOCATION.address()), 0);
        assert_eq!(
            space.get(IOMMU_L1_SB_LOCATION.address()),
            PRIMARY_SB_LOCATION
        );
        assert_eq!(
            space.get(IOMMU_L2_SB_LOCATION.address()),
            PRIMARY_SB_LOCATION
        );

        // Bridges 8..=21, 29 and 44 hidden; the bonus core's 16..=19 shown.
        for bridge in [8, 15, 20, 21, 29, 44] {
            assert_eq!(
                space.get(iohc::iohc_bridge_cntl(bridge)),
                0x0004_0005,
                "{bridge}"
            );
        }
        for bridge in 16..20 {
            assert_eq!(space.get(iohc::iohc_bridge_cntl(bridge)), 0x0004_0000);
        }
        assert_eq!(space.get(iohc::iohc_bridge_cntl(7)), 0);

        space.set(iohc::SB_LOCATION.address() + RB_SMN_STRIDE, 0x4_0001);
        rb1.early(B0, &policy);
        let s1 = RB_SMN_STRIDE;
        assert_eq!(space.get(NB_BUS_NUM_CNTL.address() + s1), 0x100);
        assert_eq!(space.get(iohc::SB_LOCATION.address() + s1), 0);
        assert_eq!(space.get(IOMMU_L1_SB_LOCATION.address() + s1), 0);
        assert_eq!(space.get(IOMMU_L2_SB_LOCATION.address() + s1), 0);

        assert_eq!(
            space.get(iohc::iohc_bridge_cntl(16).address() + s1),
            0x0004_0005
        );

        // A0 hides from bridge 0 and has no bridges 29/44.
        let a0 = SmnSpace::default();
        unsafe { RootBridge::new(&a0, 1, 0) }
            .unwrap()
            .early(A0, &policy);
        assert_eq!(
            a0.get(iohc::iohc_bridge_cntl(0).address() + s1),
            0x0004_0005
        );
        assert_eq!(a0.get(iohc::iohc_bridge_cntl(29).address() + s1), 0);

        let ctrl = iohc::iohc_bridge_cntl(3).address() + 2 * RB_SMN_STRIDE;
        space.set(ctrl, 1 << 5);
        rb2.set_bridge_visible(3, false);
        assert_eq!(space.get(ctrl), 0x0004_0005);
        rb2.set_bridge_visible(3, true);
        assert_eq!(space.get(ctrl), 0x0004_0000);
    }

    #[test]
    fn iohc_numbering_is_checked_against_the_bus_numbers() {
        use super::iohc::NB_BUS_NUM_CNTL;
        let space = SmnSpace::default();
        let bus = |iohc: u32| NB_BUS_NUM_CNTL.address() + iohc * RB_SMN_STRIDE;
        // B0's third root bridge at IOHC 3 (the model has bus 0 only).
        let third = unsafe { RootBridge::new(&space, 3, 0) }.unwrap();
        assert_eq!(third.numbering(), Numbering::Unprogrammed);
        space.set(bus(3), 0x100);
        assert_eq!(third.numbering(), Numbering::Agrees);
        // The bus is IOHC 2's: the layout's numbering is wrong.
        space.set(bus(3), 0);
        space.set(bus(2), 0x100);
        assert_eq!(
            third.numbering(),
            Numbering::Disagrees {
                bus: None,
                found: Some(2)
            }
        );
        space.set(bus(3), 0x120);
        assert_eq!(
            third.numbering(),
            Numbering::Disagrees {
                bus: Some(0x20),
                found: Some(2)
            }
        );
    }

    #[test]
    fn vdm_bus_ranges_and_ioapic_ids_follow_the_vendor() {
        use crate::df::BusRange;
        let space = SmnSpace::default();
        let nb = unsafe { RootBridge::new(&space, 1, 0) }.unwrap();
        let ranges = [
            BusRange {
                base: 0,
                limit: 0x1f,
            },
            BusRange {
                base: 0x40,
                limit: 0x5f,
            },
            BusRange {
                base: 0x80,
                limit: 0xff,
            },
        ];
        assert!(nb.set_vdm_bus_ranges(VDM_TABLE, &ranges, 1));
        let s = RB_SMN_STRIDE;
        assert_eq!(space.get(0x13b1_0190 + s), 0x8000_1f00);
        assert_eq!(space.get(0x13b1_0198 + s), 0x0000_5f40);
        assert_eq!(space.get(0x13b1_01a0 + s), 0x8000_ff80);
        assert!(nb.set_vdm_bus_ranges(VDM_TABLE_A0, &ranges, 0));
        assert_eq!(space.get(0x13b1_01a0 + s), 0x8000_5f40);
        assert!(!nb.set_vdm_bus_ranges(VDM_TABLE_A0, &[ranges[0]; 9], 0));

        assert_eq!(ioapic_id(1, 15), Some(16));
        assert_eq!(ioapic_id(0xf1, 14), Some(0xff));
        assert_eq!(ioapic_id(0xf1, 15), None);
    }

    #[test]
    fn ioapic_enable_lock_id_and_routes_use_rb_stride() {
        let space = SmnSpace::default();
        let nb = unsafe { RootBridge::new(&space, 3, 0) }.unwrap();
        assert!(!nb.has_iommu());
        assert!(nb.init_ioapic(0xfec0_1000, 0x12));
        let shift = 3 * RB_SMN_STRIDE;
        assert_eq!(space.get(IOAPIC_BAR_HI.address() + shift), 0);
        assert_eq!(space.get(IOAPIC_BAR_LO.address() + shift), 0xfec0_1003);
        assert_eq!(space.get(ioapic::IOAPIC_ID.address() + shift), 0x12 << 24);

        nb.base_init(B0, &Policy::default());
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
        // B0 leaves bridges 16..23 to their reset values.
        assert_eq!(
            space.get(ioapic::br_interrupt_routing(16).address() + shift),
            0
        );
        assert_eq!(space.get(IOAPIC_ROUTING_BASE.address() + shift), 3);
        let a0 = unsafe { RootBridge::new(&space, 1, 0) }.unwrap();
        a0.base_init(A0, &Policy::default());
        for (i, route) in ROUTES.iter().enumerate() {
            let raw = space.get(ioapic::br_interrupt_routing(i).address() + RB_SMN_STRIDE);
            let expect =
                u32::from(route.group) | u32::from(route.swizzle) << 4 | u32::from(route.map) << 16;
            assert_eq!(raw, expect, "bridge {i}");
        }
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
            0xfed8_0000
        );
        rb0.enable_iommu_mmio().unwrap();
        assert_eq!(
            unsafe { pci.read(PciAddress::new(0, 0, 0, 2), IOMMU_CAP_BASE_LO) },
            0xfed8_0001
        );
        rb0.set_iommu_vf_bars(0x2000_0000, Some(0x3040_0000))
            .unwrap();
        assert_eq!(
            unsafe { pci.read(PciAddress::new(0, 0, 0, 2), 0x110) },
            0x2000_0001
        );
        assert_eq!(
            unsafe { pci.read(PciAddress::new(0, 0, 0, 2), 0x118) },
            0x3040_0001
        );
        assert_eq!(
            rb0.set_iommu_vf_bars(0x2000_0000, None),
            Err(bars::BarError::Taken)
        );
        assert!(!rb1.map_iommu(0xfed8_0000)); // cap ID absent on rb1
        assert!(!rb2.set_iommu_bar(0xfed8_0000));
        assert!(unsafe { RootBridge::new(&pci, 3, 0) }.is_some());
        assert!(unsafe { RootBridge::new(&pci, 4, 0) }.is_none());

        let model = crate::smn::model::Model::default();
        let rb1_smn = unsafe { RootBridge::new(model.clone(), 1, 0) }.unwrap();
        rb1_smn.iommu_init(B0, &Policy::default(), &mut Fuses::default());
        let s = RB_SMN_STRIDE;
        let get = |a: u32| model.0.borrow().smn.get(&(0, a)).copied().unwrap_or(0);
        assert_eq!(get(0x13f0_1200 + s), 1);
        assert_eq!(get(0x13f0_1250 + s), 0x4949);
        assert_eq!(get(0x1470_00dc + s), 0x20);
        assert_eq!(get(0x1530_02d8 + s), 0x1_ffff);
    }

    #[test]
    fn ioapic_and_iommu_bar_verify_readback_lock_and_unavailable() {
        use crate::smn::testing::{SmnModel, SmnTest};

        // Locked to matching address succeeds; locked to different address fails.
        let space = SmnSpace::default();
        let nb = unsafe { RootBridge::new(&space, 0, 0) }.unwrap();
        assert!(nb.init_ioapic(0xfec0_1000, 0x11));
        assert!(nb.init_ioapic(0xfec0_1000, 0x12));
        assert_eq!(space.get(ioapic::IOAPIC_ID.address()), 0x12 << 24);
        assert!(!nb.init_ioapic(0xfec0_2000, 0x13));
        assert!(!nb.init_ioapic(0xfec0_1001, 0x14));

        // Unavailable aperture (all-ones) is rejected.
        struct Dead;
        impl SmnModel for Dead {
            fn read(&mut self, _: u32) -> u32 {
                u32::MAX
            }
            fn write(&mut self, _: u32, _: u32) {}
        }
        let dead = SmnTest::new(Dead);
        let dead_nb = unsafe { RootBridge::new(&dead, 0, 0) }.unwrap();
        assert!(!dead_nb.init_ioapic(0xfec0_1000, 0x11));

        // IOMMU PCI config readback all-ones fails.
        struct DeadPci;
        impl ConfigRegionAccess for DeadPci {
            unsafe fn read(&self, _: PciAddress, _: u16) -> u32 {
                u32::MAX
            }
            unsafe fn write(&self, _: PciAddress, _: u16, _: u32) {}
        }
        let rb = unsafe { RootBridge::new(&DeadPci, 0, 0) }.unwrap();
        assert!(!rb.map_iommu(0xfed8_0000));
        assert!(!rb.set_iommu_bar(0xfed8_0000));
    }
}
