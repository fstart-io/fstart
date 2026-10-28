//! Naples (ZP) NBIO: one per die, up to 8, each through its own root bus
//! at the same SMN addresses (NBIO_SMU §3.1, §3.3).
//!
//! The Naples PEIM is the SP4r2/MJ11 code with renumbered PCDs, the SP3
//! property word and one more NBIF2 strap record; the direct steps are
//! ../sp4r2 NBIO_BASE_ZP §3.3, run on every die.
//!
//! Order per die (`AmdNbioBaseInit` `ffde3e25`):
//! [`base_init`] (NBIF bus numbers, tables, revision strap, IOAPIC features
//! and bus number, parity/poison workarounds, IOMMU RAS copies), then on
//! the memory-info PPI [`Nbio::set_tom`], at PCI enumeration the IOAPIC /
//! IOMMU / PSP (die ≥ 1) BARs, and after enumeration
//! [`Nbio::nbif_hw_init_lock`].

#[rustfmt::skip]
pub mod tables;

use fstart_pci::ConfigRegionAccess;

use super::{Nbio, NbioPolicy};
use crate::sp3::soc::Revision;

/// Property bits of the legacy tables (../sp4r2 NBIO_BASE_ZP §3.2).
pub mod property {
    /// IOMMU disabled (PCD 0x02 == 0).
    pub const IOMMU_OFF: u32 = 0x2;
    /// SST clock gating disabled (PCD 0x8B == 0).
    pub const SST_CG_OFF: u32 = 0x4;
    /// IOMMU L1 clock gating (PCD 0x86).
    pub const IOMMU_L1_CG: u32 = 0x40;
    /// IOMMU L2 clock gating (PCD 0x87).
    pub const IOMMU_L2_CG: u32 = 0x80;
    /// Package in {SP4, SP4r2, SP3}: always set here.
    pub const SERVER_PACKAGE: u32 = 0x800;
    /// IOHC clock gating (PCD 0x88).
    pub const IOHC_CG: u32 = 0x2000_0000;
    /// RAS / poison (PCD 0x14).
    pub const RAS: u32 = 0x4000_0000;
}

/// The live property word for `policy` (SP3 defaults: 0x200008C0).
pub fn property(policy: &NbioPolicy) -> u32 {
    use property::*;
    [
        (!policy.iommu, IOMMU_OFF),
        (!policy.sst_clock_gating, SST_CG_OFF),
        (policy.iommu_l1_clock_gating, IOMMU_L1_CG),
        (policy.iommu_l2_clock_gating, IOMMU_L2_CG),
        (true, SERVER_PACKAGE),
        (policy.iohc_clock_gating, IOHC_CG),
        (policy.naples.ras_poison, RAS),
    ]
    .iter()
    .filter(|(on, _)| *on)
    .fold(0, |p, (_, bit)| p | bit)
}

/// NBIF0 strap written on ZP-A0..B1 only.
const NBIF0_460C: u32 = 0x1013_460C;
/// `NBRASCFG_0400` (poison control).
const NBRASCFG_0400: u32 = 0x13B2_0400;

/// The die's PEI base init, in AGESA order. `ioapic_base`: Naples PCD
/// 0x8D (die 0 only, default unset); normally the IOAPIC BAR is placed at
/// PCI enumeration with [`Nbio::set_ioapic_bar`].
pub fn base_init<P: ConfigRegionAccess + Clone>(
    nbio: &Nbio<P>,
    policy: &NbioPolicy,
    revision: Revision,
    ioapic_base: Option<u64>,
) {
    nbio.set_nbif_bus_numbers(policy);
    let live = property(policy);
    nbio.run_legacy(tables::BASE, live);
    nbio.run_legacy(tables::IOMMU, live);
    if matches!(revision, Revision::ZpA0 | Revision::ZpB0 | Revision::ZpB1) {
        nbio.modify32(
            NBIF0_460C,
            1 << 20,
            (policy.naples.nbif0_460c_bit20 as u32) << 20,
        );
    }
    if let (Some(base), true) = (ioapic_base, nbio.fch && nbio.socket == 0) {
        nbio.set_ioapic_bar(base);
    }
    nbio.ioapic_features();
    nbio.set_bus_number();
    if policy.naples.nbif_parity_workaround {
        (0..3).for_each(|k| nbio.modify32(nbio.nbif(k) + super::nbif::ZP_PARITY, 0, 0x77));
    }
    // NbioNBIFPoisonDataWorkaroundZP: NBIF ALT.
    [0x0141_F800, 0x0151_F800, 0x0151_F804]
        .iter()
        .for_each(|&a| nbio.modify32(a, 0, 0x77));
    // IOMMU RAS: mirror D0F2x80 and 0x0240001C into the L2A copies.
    let copy = |to, from| {
        if let Some(v) = nbio.read32(from) {
            nbio.write32(to, v)
        }
    };
    copy(super::iommu::L2A_EFR_SHADOW0, super::iommu::EFR_SHADOW0);
    copy(0x1570_401C, 0x0240_001C);
    if policy.naples.smn_04000200_bit16 {
        nbio.modify32(0x0400_0200, 0, 1 << 16);
    }
    if !policy.naples.ras_poison {
        // The RAS table record (`|= 0x191919`) is skipped; this form
        // replaces it (`ffde3cf5`, NBIO_SMU §3.3).
        nbio.modify32(NBRASCFG_0400, 0x1900, 0x0019_0019);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sp3::nbio::tests::nbio;
    use crate::sp3::soc::Generation;

    #[test]
    fn sp3_default_property() {
        assert_eq!(property(&NbioPolicy::default()), 0x2000_08C0);
    }

    #[test]
    fn base_init_die1() {
        let (m, n) = nbio(Generation::Naples, 1, 0x20);
        m.0.borrow_mut().smn.insert((0x20, NBRASCFG_0400), 0x1900);
        base_init(
            &n,
            &NbioPolicy::default(),
            Revision::ZpB2,
            Some(0xFEC0_1000),
        );
        let s = m.0.borrow();
        assert_eq!(s.smn[&(0x20, 0x1430_0000)], 0x34, "die 1: SECONDARY");
        assert_eq!(s.smn[&(0x20, 0x13B1_0044)], 0x120);
        assert_eq!(s.smn[&(0x20, NBRASCFG_0400)], 0x0019_0019);
        assert_eq!(
            s.smn[&(0x20, 0x1033_1400)] >> 18 & 0xFF,
            0x29,
            "NBIF2 strap"
        );
        assert_eq!(s.smn[&(0x20, 0x1023_1404)] >> 24, 0x20, "bus strap");
        assert!(!s.smn.contains_key(&(0x20, NBIF0_460C)), "B2: no strap");
        assert!(
            !s.smn.contains_key(&(0x20, 0x13B1_02F0)),
            "BAR only on die 0"
        );
    }
}
