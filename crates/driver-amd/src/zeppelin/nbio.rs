// SPDX-License-Identifier: MIT OR Apache-2.0
//! NBIO base initialization of the vendor AmdNbioBaseZPPei and the late
//! settings of AmdNbioBaseZPDxe/AmdNbioPcieZPDxe, per die (sp4r2 report
//! NBIO_BASE_ZP and nbio-base-contracts.json).
use super::{Die, Revision};
use crate::fam17h::registers::{Smn, SmnReg};
use fstart_pci::ConfigRegionAccess;
use tock_registers::{
    interfaces::{ReadWriteable, Writeable},
    register_bitfields,
};

pub use super::nbio_tables::{BASE, IOMMU};

pub use crate::fam17h::registers::{Reg, apply, copy, rmw, write};
pub(super) use crate::fam17h::nbio::*;

register_bitfields![u32,
    NbifBusCfg [BUS OFFSET(24) NUMBITS(8) []],
    NbifRevStrap [
        /// Vendor PCD 0xb9.
        PCD_B9 OFFSET(20) NUMBITS(1) []
    ],
    NbifHwinitLock [LOCK OFFSET(0) NUMBITS(1) []]
];

/// A die has two PCIe cores (core 0: lanes 0-15, devices 1.x and 2.1; core
/// 1: lanes 16-31, devices 3.x and 4.1).
pub const PCIE_CORE_COUNT: usize = 2;
pub const PCIE_PORT_COUNT: usize = PCIE_CORE_COUNT * PCIE_PORTS_PER_CORE;

/// PCIe strap `strap` of core `core`, behind the core's strap index/data
/// pair in SMN.
pub(super) fn strap<P: ConfigRegionAccess>(
    smn: &Smn<P>,
    core: usize,
    strap: u32,
) -> fstart_core::indexed::Indexed<
    crate::fam17h::registers::SmnRegister<'_, P, ()>,
    crate::fam17h::registers::SmnRegister<'_, P, ()>,
> {
    fstart_core::indexed::Indexed::new(
        smn.reg(pcie::strap_index(core)),
        smn.reg(pcie::strap_data(core)),
        strap,
    )
}

/// NBIF (SMN): three instances 0x101xxxxx, 0x102xxxxx, 0x103xxxxx.
pub(super) const NBIF_COUNT: u32 = 3;
pub(super) const fn nbif_reg(n: u32, offset: u32) -> u32 {
    0x1010_0000 + n * 0x10_0000 + offset
}
/// Bits 31:24: root bus of the die.
const fn nbif_bus_cfg(n: u32) -> SmnReg<NbifBusCfg::Register> {
    SmnReg::new(nbif_reg(n, 0x31404))
}
const fn nbif_hwinit_lock(n: u32) -> SmnReg<NbifHwinitLock::Register> {
    SmnReg::new(nbif_reg(n, 0x2358c))
}
const NBIF_REV_STRAP: SmnReg<NbifRevStrap::Register> = SmnReg::new(0x1013_460c);

/// Parity and poison handling of the NBIFs and two other blocks.
fn parity_poison() -> impl Iterator<Item = Reg> {
    (0..NBIF_COUNT)
        .map(|n| nbif_reg(n, 0x39004))
        .chain([0x0141_f800, 0x0151_f800, 0x0151_f804])
        .map(|address| rmw(address, 0, 0x77))
}

/// IOMMU RAS.
const IOMMU_RAS: &[Reg] = &[
    copy(0x1570_4330, 0x13f0_0080),
    copy(0x1570_401c, 0x0240_001c),
    rmw(0x0400_0200, 0, 1 << 16),
];

/// AmdNbioBaseZPPei for one die: the register tables, bus number, IOAPIC
/// features, parity/poison handling and IOMMU RAS.
pub fn early_init<P: ConfigRegionAccess + Copy>(die: &Die<P>, revision: Revision) {
    die_init(&die.smn(), die.index(), die.bus(), revision);
}

fn die_init(smn: &Smn<impl ConfigRegionAccess>, die: u8, bus: u8, revision: Revision) {
    for n in 0..NBIF_COUNT {
        smn.reg(nbif_bus_cfg(n))
            .modify(NbifBusCfg::BUS.val(bus.into()));
    }

    apply(smn, BASE.iter().copied());
    apply(smn, IOMMU.iter().copied());

    // ZP A0/B0/B1 only: vendor PCD 0xb9 (0).
    if matches!(revision, Revision::A0 | Revision::B0 | Revision::B1) {
        smn.reg(NBIF_REV_STRAP).modify(NbifRevStrap::PCD_B9::CLEAR);
    }

    let features = IoapicFeatures::ID_EXT::SET + IoapicFeatures::FCH::SET;
    smn.reg(IOAPIC_FEATURES_ENABLE).modify(if die != 0 {
        features + IoapicFeatures::SECONDARY::SET
    } else {
        features
    });
    smn.reg(NB_BUS_NUM_CNTL)
        .write(NbBusNumCntl::LAT_MODE::SET + NbBusNumCntl::BUS.val(u32::from(bus)));

    apply(smn, parity_poison());
    apply(smn, IOMMU_RAS.iter().copied());
}

/// AmdNbioBaseZPDxe/AmdNbioPcieZPDxe before PCI enumeration: PCIe core clock
/// gating, then lock the NBIF hardware init registers (straps, IDs).
pub fn pre_enumeration_init<P: ConfigRegionAccess + Copy>(die: &Die<P>) {
    let smn = die.smn();
    // The per-die wrapper features of the vendor topology template enable
    // ClkGating but not LclkGating.
    for core in 0..PCIE_CORE_COUNT {
        smn.reg(pcie::config_cntl(core))
            .modify(pcie::ConfigCntl::DYN_CLK_LATENCY.val(0xf));
        apply(&smn, [rmw(pcie::cpm_control(core), 0x3c00, 0x81e6)]);
        smn.reg(pcie::cntl_18c(core))
            .modify(pcie::Cntl18c::GATING.val(3));
    }
    for n in 0..NBIF_COUNT {
        smn.reg(nbif_hwinit_lock(n))
            .modify(NbifHwinitLock::LOCK::SET);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fam17h::registers::testing::SmnSpace;

    #[test]
    fn tables_copy_write_and_update_in_order() {
        let space = SmnSpace::default();
        space.set(0x10_u32, 0xf0f0);
        space.set(0x20_u32, 0x1234);
        apply(
            &space.smn(),
            [
                rmw(0x10, 0xff, 0x01),
                copy(0x14, 0x10),
                write(0x10, 0x5),
                copy(0x18, 0x20),
            ],
        );
        assert_eq!(space.get(0x10_u32), 5);
        assert_eq!(space.get(0x14_u32), 0xf001);
        assert_eq!(space.get(0x18_u32), 0x1234);
    }

    #[test]
    fn die_init_programs_bus_and_revision_strap() {
        let space = SmnSpace::default();
        space.set(NBIF_REV_STRAP, 1 << 20);
        space.set(nbif_bus_cfg(2), 0x1200_0034);
        die_init(&space.smn(), 1, 0x40, Revision::B1);
        assert_eq!(space.get(NBIF_REV_STRAP), 0);
        assert_eq!(space.get(nbif_bus_cfg(2)), 0x4000_0034);
        assert_eq!(space.get(NB_BUS_NUM_CNTL), 0x140);
        assert_eq!(space.get(IOAPIC_FEATURES_ENABLE), 0x34);
        assert_eq!(space.get(0x0151_f804_u32), 0x77);

        let space = SmnSpace::default();
        space.set(NBIF_REV_STRAP, 1 << 20);
        die_init(&space.smn(), 0, 0, Revision::B2);
        assert_eq!(space.get(NBIF_REV_STRAP), 1 << 20);
        assert_eq!(space.get(IOAPIC_FEATURES_ENABLE), 0x14);
    }
}
