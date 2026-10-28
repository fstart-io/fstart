//! The NBIO IOMMU (D0F2): MMIO BAR, enable, EFR shadow, L1/L2 addresses and
//! the data IVRS needs (NBIO_SMU §3.7). The L1/L2 init values are in the
//! generated IOMMU tables.
//!
//! D0F2 config registers are reached through their SMN alias at
//! `0x13F0_0000` (+ quadrant), like the AMI root-bridge driver does.

use fstart_pci::ConfigRegionAccess;
use tock_registers::interfaces::{ReadWriteable, Writeable};
use tock_registers::register_bitfields;

use super::{Nbio, NbioPolicy};
use crate::smn::SmnAddr;
use crate::sp3::soc::Generation;

/// SMN alias of the IOMMU's PCI config space (D0F2).
pub const L2_CONFIG: SmnAddr = 0x13F0_0000;
/// IOMMU capability offset (cap ID 0x0F).
pub const CAP_OFFSET: u8 = 0x40;
/// D0F2x44 `IOMMU_CAP_BASE_LO` (cap + 4).
pub const CAP_BASE_LO: SmnAddr = L2_CONFIG + 0x44;
/// D0F2x48 `IOMMU_CAP_BASE_HI` (cap + 8).
pub const CAP_BASE_HI: SmnAddr = L2_CONFIG + 0x48;
/// D0F2x80: writable EFR shadow (`IOMMU_MMIO_CONTROL0_W`, INF).
pub const EFR_SHADOW0: SmnAddr = L2_CONFIG + 0x80;
/// D0F2x84: second EFR shadow word (INF).
pub const EFR_SHADOW1: SmnAddr = L2_CONFIG + 0x84;
/// L2A copy of the EFR shadow words (Milan SNP bits).
pub const L2A_EFR_SHADOW0: SmnAddr = 0x1570_4330;
pub const L2A_EFR_SHADOW1: SmnAddr = 0x1570_4334;
/// `L2_CP_CONTROL_1`.
pub const L2_CP_CONTROL_1: SmnAddr = 0x13F0_115C;
/// `L2_SB_LOCATION`.
pub const L2_SB_LOCATION: SmnAddr = 0x13F0_112C;

/// MMIO size of one IOMMU.
pub const MMIO_SIZE: u64 = 512 << 10;

/// L1 instances on Rome/Milan: PCIE0, PCIE1, NBIF, IOAGR.
pub const L1_SP3: [SmnAddr; 4] = [0x1470_0000, 0x14B0_0000, 0x14F0_0000, 0x1530_0000];
/// L1 instances on Naples (names not recovered).
pub const L1_NAPLES: [SmnAddr; 4] = [0x1470_0000, 0x1480_0000, 0x1490_0000, 0x14A0_0000];
/// The L1 that holds `L1_SB_LOCATION` (+0x24): IOAGR on Rome/Milan.
pub const L1_IOAGR_SP3: SmnAddr = 0x1530_0000;
/// `L1_SB_LOCATION` offset.
pub const L1_SB_LOCATION: SmnAddr = 0x24;
/// L1 offset whose bit 4 Milan's SNP policy clears (unnamed).
pub const L1_0DC: SmnAddr = 0xDC;

register_bitfields![u32,
    pub BASE_LO [
        ADDR OFFSET(19) NUMBITS(13) [],
        EN OFFSET(0) NUMBITS(1) []
    ],
    pub BASE_HI [
        ADDR OFFSET(0) NUMBITS(32) []
    ],
    pub SHADOW0 [
        /// EFR XTSup (INF).
        XT OFFSET(2) NUMBITS(1) [],
        /// EFR GASup (INF).
        GA OFFSET(7) NUMBITS(1) [],
        /// Milan SNP-style bits 4, 5 (cleared by PCD 0x1C2).
        SNP_LO OFFSET(4) NUMBITS(2) [],
        /// EFR GAMSup (INF).
        GAM OFFSET(21) NUMBITS(3) []
    ],
    pub SHADOW1 [
        BIT21_22 OFFSET(21) NUMBITS(2) [],
        BIT31 OFFSET(31) NUMBITS(1) []
    ]
];

/// What IVRS needs about one IOMMU (the IVHD header; the feature words
/// come from the IOMMU's own capability and MMIO registers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IommuInfo {
    pub socket: u8,
    pub index: u8,
    /// IOMMU device ID `bus:00.2`.
    pub bus: u8,
    pub devfn: u8,
    pub cap_offset: u8,
    pub base: u64,
    pub segment: u16,
    /// The NBIO has the FCH: IVHD gets the FCH device entries.
    pub fch: bool,
}

impl<P: ConfigRegionAccess + Clone> Nbio<P> {
    /// The L1 instances of this NBIO.
    pub fn iommu_l1s(&self) -> [SmnAddr; 4] {
        let l1 = match self.generation {
            Generation::Naples => L1_NAPLES,
            Generation::Rome | Generation::Milan => L1_SP3,
        };
        l1.map(|a| self.at(a))
    }

    /// Place the IOMMU MMIO (512 KiB, 512 KiB aligned, below 4 GiB on Milan's
    /// AGESA path) without enabling it (`gn.iommu_bar`, AMI PciRootBridge).
    pub fn set_iommu_bar(&self, base: u64) {
        debug_assert_eq!(base % MMIO_SIZE, 0);
        self.reg::<BASE_HI::Register>(self.at(CAP_BASE_HI))
            .write(BASE_HI::ADDR.val((base >> 32) as u32));
        self.reg::<BASE_LO::Register>(self.at(CAP_BASE_LO))
            .write(BASE_LO::ADDR.val((base >> 19) as u32 & 0x1FFF));
    }

    /// The IOMMU MMIO base, if placed.
    pub fn iommu_bar(&self) -> Option<u64> {
        let lo = self.read32(self.at(CAP_BASE_LO))? & 0xFFF8_0000;
        let hi = self.read32(self.at(CAP_BASE_HI))? as u64;
        let base = hi << 32 | lo as u64;
        (base != 0).then_some(base)
    }

    /// Enable the IOMMU when its BAR is placed (IOMMU DXE: cap+4 |= 1,
    /// S3-saved by AGESA). Returns whether it was enabled.
    pub fn iommu_enable(&self) -> bool {
        if self.iommu_bar().is_none() {
            return false;
        }
        self.reg::<BASE_LO::Register>(self.at(CAP_BASE_LO))
            .modify(BASE_LO::EN::SET);
        true
    }

    /// The IOMMU-PEI direct steps after the IOMMU table: EFR shadow XT/GA
    /// (Rome PCD 0xE, Milan PCD 0x2C), Milan's SNP/GAM bits, CP control and
    /// per-NBIO enable (NBIO_SMU §3.7, `gn.iommu_snp_bits`,
    /// `gn.iommu_enable_pcds`).
    pub fn iommu_post_table(&self, policy: &NbioPolicy) {
        let shadow0 = self.reg::<SHADOW0::Register>(self.at(EFR_SHADOW0));
        if self.generation != Generation::Naples && policy.iommu_xt_ga(self.generation) {
            shadow0.modify(SHADOW0::XT::SET + SHADOW0::GA::SET);
        }
        if self.generation != Generation::Milan {
            return;
        }
        let m = &policy.milan;
        let l2a0 = self.reg::<SHADOW0::Register>(self.at(L2A_EFR_SHADOW0));
        let shadow1 = self.reg::<SHADOW1::Register>(self.at(EFR_SHADOW1));
        if m.iommu_snp_clear {
            let clear = SHADOW0::SNP_LO::CLEAR + SHADOW0::GAM::CLEAR;
            shadow0.modify(clear);
            l2a0.modify(clear);
            self.iommu_l1s()
                .iter()
                .for_each(|&l1| self.modify32(l1 + L1_0DC, 1 << 4, 0));
            shadow1.modify(SHADOW1::BIT21_22::CLEAR);
            self.modify32(self.at(L2A_EFR_SHADOW1), 1 << 21, 0);
        } else {
            shadow1.modify(SHADOW1::BIT31::CLEAR);
            shadow0.modify(SHADOW0::GAM.val(m.iommu_gam as u32));
            l2a0.modify(SHADOW0::GAM.val(m.iommu_gam as u32));
        }
        if m.iommu_cp_control1_bit2 {
            self.modify32(self.at(L2_CP_CONTROL_1), 0, 1 << 2);
        }
        let per_nbio = m
            .iommu_per_nbio
            .get(self.socket as usize)
            .and_then(|s| s.get(self.index as usize))
            .copied()
            .unwrap_or(true);
        if !per_nbio {
            self.disable_iommu();
        }
    }

    /// IVRS data for this NBIO's IOMMU, if it is placed.
    pub fn iommu_info(&self) -> Option<IommuInfo> {
        Some(IommuInfo {
            socket: self.socket,
            index: self.index,
            bus: self.bus(),
            devfn: 0x02,
            cap_offset: CAP_OFFSET,
            base: self.iommu_bar()?,
            segment: 0,
            fch: self.fch,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sp3::nbio::tests::nbio;

    #[test]
    fn bar_and_enable() {
        let (m, n) = nbio(Generation::Rome, 3, 0x60);
        assert!(!n.iommu_enable());
        n.set_iommu_bar(0xB008_0000);
        assert!(n.iommu_enable());
        assert_eq!(m.0.borrow().smn[&(0x60, 0x1420_0044)], 0xB008_0001);
        let info = n.iommu_info().unwrap();
        assert_eq!((info.bus, info.devfn, info.base), (0x60, 2, 0xB008_0000));
    }

    #[test]
    fn efr_shadow_defaults() {
        let (m, r) = nbio(Generation::Rome, 0, 0);
        r.iommu_post_table(&NbioPolicy::default());
        assert!(
            !m.0.borrow().smn.contains_key(&(0, EFR_SHADOW0)),
            "Rome: off"
        );

        let (m, g) = nbio(Generation::Milan, 0, 0);
        m.0.borrow_mut().smn.insert((0, EFR_SHADOW1), 0x8000_0000);
        g.iommu_post_table(&NbioPolicy::default());
        let s = m.0.borrow();
        assert_eq!(s.smn[&(0, EFR_SHADOW0)], 0x84);
        assert_eq!(s.smn[&(0, EFR_SHADOW1)], 0);
    }
}
