// SPDX-License-Identifier: MIT OR Apache-2.0
//! NBIO base initialization of the vendor AmdNbioBaseZPPei and the late
//! settings of AmdNbioBaseZPDxe/AmdNbioPcieZPDxe, per die (sp4r2 report
//! NBIO_BASE_ZP and nbio-base-contracts.json; the SP3 PEIM is the same
//! code with renumbered PCDs, NBIO_SMU §3.3).
use super::{LegacyEntry, property, tables};
use crate::cpu::Revision;
use crate::nbio::{
    ioapic::{IOAPIC_FEATURES_ENABLE, IoapicFeatures},
    iohc::{NB_BUS_NUM_CNTL, NbBusNumCntl},
    pcie::{self, PCIE_PORTS_PER_CORE},
};
use crate::{
    df::Die,
    smn::{Reg, Smn, SmnReg, apply, copy, rmw},
    soc::Soc,
};
use fstart_pci::ConfigRegionAccess;
use tock_registers::{
    interfaces::{ReadWriteable, Writeable},
    register_bitfields,
};

/// What AmdNbioBaseZPPei does depends on (vendor PCDs). [`Self::SP4`] is
/// the SP4/SP4r2 vendor default, [`Self::SP3`] Naples'.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZpPolicy {
    /// The live property word that selects the tables' conditional records.
    pub property: u32,
    /// The SP3 PEIM's extra NBIF2 strap record ([`tables::BASE_NAPLES`]).
    pub sp3: bool,
    /// NBIF0 0x460C bit 20 on ZP-A0..B1 (SP4 PCD 0xb9, SP3 0x84).
    pub rev_strap: bool,
    /// The NBIF parity workaround.
    pub parity: bool,
    /// SMN 0x04000200 bit 16 of the IOMMU RAS step.
    pub iommu_ras_bit16: bool,
}

impl ZpPolicy {
    /// IOMMU and SST clock gating on, RAS on.
    pub const SP4: Self = Self {
        property: property::IOMMU_L1_CG
            | property::IOMMU_L2_CG
            | property::SERVER_PACKAGE
            | property::IOHC_CG
            | property::RAS,
        sp3: false,
        rev_strap: false,
        parity: true,
        iommu_ras_bit16: true,
    };
}

/// The property word of the ZP tables on a Zeppelin SoC.
pub const PROPERTY: u32 = ZpPolicy::SP4.property;

/// The records of `table` under the live `property`, in order.
fn records(table: &'static [LegacyEntry], property: u32) -> impl Iterator<Item = Reg> {
    table
        .iter()
        .filter(move |entry| entry.applies(property))
        .map(LegacyEntry::record)
}

/// AmdNbioBaseZPPei's base table as a Zeppelin SoC runs it.
pub fn base() -> impl Iterator<Item = Reg> {
    records(tables::BASE, PROPERTY)
}

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
pub(crate) fn strap<P: ConfigRegionAccess>(
    smn: &Smn<P>,
    core: usize,
    strap: u32,
) -> fstart_core::indexed::Indexed<
    crate::smn::SmnRegister<'_, P, ()>,
    crate::smn::SmnRegister<'_, P, ()>,
> {
    fstart_core::indexed::Indexed::new(
        smn.reg(pcie::strap_index(core)),
        smn.reg(pcie::strap_data(core)),
        strap,
    )
}

/// NBIF (SMN): three instances 0x101xxxxx, 0x102xxxxx, 0x103xxxxx.
pub(crate) const NBIF_COUNT: u32 = 3;
pub(crate) const fn nbif_reg(n: u32, offset: u32) -> u32 {
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

/// The NBIF parity workaround.
fn parity() -> impl Iterator<Item = Reg> {
    (0..NBIF_COUNT).map(|n| rmw(nbif_reg(n, 0x39004), 0, 0x77))
}

/// NbioNBIFPoisonDataWorkaroundZP: NBIF ALT.
fn poison() -> impl Iterator<Item = Reg> {
    [0x0141_f800, 0x0151_f800, 0x0151_f804]
        .into_iter()
        .map(|address| rmw(address, 0, 0x77))
}

/// IOMMU RAS: the L2A copies of D0F2x80 and 0x0240001C.
const IOMMU_RAS: &[Reg] = &[
    copy(0x1570_4330, 0x13f0_0080),
    copy(0x1570_401c, 0x0240_001c),
];

/// `NBRASCFG_0400` (poison control) without the RAS records: AGESA's
/// replacement for them (`ffde3cf5`).
const NO_RAS_POISON: Reg = rmw(0x13b2_0400, 0x1900, 0x0019_0019);

/// AmdNbioBaseZPPei for one die of a SoC with the SP4 policy.
pub fn early_init<S: Soc, P: ConfigRegionAccess + Copy>(die: &Die<S, P>, revision: Revision) {
    base_init(
        &die.smn(),
        die.index() == 0,
        die.bus(),
        revision,
        &ZpPolicy::SP4,
    );
}

/// AmdNbioBaseZPPei for one die with root bus `bus`: the register tables,
/// bus number, IOAPIC features (`primary`: the die with the system's FCH),
/// parity/poison handling and IOMMU RAS.
pub fn base_init(
    smn: &Smn<impl ConfigRegionAccess>,
    primary: bool,
    bus: u8,
    revision: Revision,
    policy: &ZpPolicy,
) {
    for n in 0..NBIF_COUNT {
        smn.reg(nbif_bus_cfg(n))
            .modify(NbifBusCfg::BUS.val(bus.into()));
    }

    apply(smn, records(tables::BASE, policy.property));
    if policy.sp3 {
        apply(smn, records(tables::BASE_NAPLES, policy.property));
    }
    apply(smn, records(tables::IOMMU, policy.property));

    // ZP A0/B0/B1 only.
    if matches!(revision, Revision::ZpA0 | Revision::ZpB0 | Revision::ZpB1) {
        smn.reg(NBIF_REV_STRAP)
            .modify(NbifRevStrap::PCD_B9.val(policy.rev_strap.into()));
    }

    let features = IoapicFeatures::ID_EXT::SET + IoapicFeatures::FCH::SET;
    smn.reg(IOAPIC_FEATURES_ENABLE).modify(if primary {
        features
    } else {
        features + IoapicFeatures::SECONDARY::SET
    });
    smn.reg(NB_BUS_NUM_CNTL)
        .write(NbBusNumCntl::LAT_MODE::SET + NbBusNumCntl::BUS.val(u32::from(bus)));

    if policy.parity {
        apply(smn, parity());
    }
    apply(smn, poison());
    apply(smn, IOMMU_RAS.iter().copied());
    if policy.iommu_ras_bit16 {
        apply(smn, [rmw(0x0400_0200, 0, 1 << 16)]);
    }
    if policy.property & property::RAS == 0 {
        apply(smn, [NO_RAS_POISON]);
    }
}

/// AmdNbioBaseZPDxe/AmdNbioPcieZPDxe before PCI enumeration: PCIe core clock
/// gating, then lock the NBIF hardware init registers (straps, IDs).
pub fn pre_enumeration_init<S: Soc, P: ConfigRegionAccess + Copy>(die: &Die<S, P>) {
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
    use crate::smn::{testing::SmnSpace, write};

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

    /// The SP4r2 PEIM's tables: 156 of its 158 base records and 36 of the
    /// 38 IOMMU records apply with its defaults.
    #[test]
    fn zeppelin_runs_the_zp_tables_without_the_iommu_off_and_sst_records() {
        assert_eq!(base().count(), 156);
        assert_eq!(records(tables::IOMMU, PROPERTY).count(), 36);
        assert_eq!(PROPERTY, 0x6000_08c0);
    }

    #[test]
    fn base_init_programs_bus_and_revision_strap() {
        let space = SmnSpace::default();
        space.set(NBIF_REV_STRAP, 1 << 20);
        space.set(nbif_bus_cfg(2), 0x1200_0034);
        base_init(&space.smn(), false, 0x40, Revision::ZpB1, &ZpPolicy::SP4);
        assert_eq!(space.get(NBIF_REV_STRAP), 0);
        assert_eq!(space.get(nbif_bus_cfg(2)), 0x4000_0034);
        assert_eq!(space.get(NB_BUS_NUM_CNTL), 0x140);
        assert_eq!(space.get(IOAPIC_FEATURES_ENABLE), 0x34);
        assert_eq!(space.get(0x0151_f804_u32), 0x77);

        let space = SmnSpace::default();
        space.set(NBIF_REV_STRAP, 1 << 20);
        base_init(&space.smn(), true, 0, Revision::ZpB2, &ZpPolicy::SP4);
        assert_eq!(space.get(NBIF_REV_STRAP), 1 << 20);
        assert_eq!(space.get(IOAPIC_FEATURES_ENABLE), 0x14);
    }

    /// The SP3 policy of a Naples die 1: its extra NBIF2 strap, the RAS
    /// poison replacement and no 0x04000200 bit.
    #[test]
    fn naples_die() {
        let space = SmnSpace::default();
        space.set(0x13b2_0400_u32, 0x1900);
        let policy = ZpPolicy {
            property: ZpPolicy::SP4.property & !property::RAS,
            sp3: true,
            iommu_ras_bit16: false,
            ..ZpPolicy::SP4
        };
        base_init(&space.smn(), false, 0x20, Revision::ZpB2, &policy);
        assert_eq!(space.get(0x13b2_0400_u32), 0x0019_0019);
        assert_eq!(space.get(0x1033_1400_u32) >> 18 & 0xff, 0x29, "NBIF2 strap");
        assert_eq!(space.get(0x1023_1404_u32) >> 24, 0x20, "bus strap");
        assert_eq!(space.get(0x0400_0200_u32), 0);
        assert_eq!(space.get(IOAPIC_FEATURES_ENABLE), 0x34, "SECONDARY");
    }
}
