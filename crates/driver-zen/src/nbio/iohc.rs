//! IOHC registers shared by ZP, SSP and GN, and the IOHC steps that do not
//! differ beyond what [`Nbio::at`] covers: TOM/TOM2/TOM3, bus number, the
//! SMU/PSP/IOAPIC BARs (NBIO_SMU §3.5, PSP_APOB §3.2).
//!
//! Addresses are IOHC0 / die addresses; use [`Nbio::at`] for a quadrant.
//! Register names are the public ones (PPR, Linux, illumos).

use fstart_pci::{ConfigRegionAccess, ConfigRegister};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::register_bitfields;

use super::Nbio;
use crate::smn::SmnAddr;
use crate::soc::Generation;

/// `NB_PCI_CTRL` (D0F0x4C alias).
pub const NB_PCI_CTRL: SmnAddr = 0x13B0_004C;
/// D0F0x50 alias (subsystem IDs; Milan table writes [31:16]).
pub const NB_ADAPTER_ID_W: SmnAddr = 0x13B0_0050;
/// IOHC IOMMU enable (unnamed, NBIO_SMU §3.4: cleared when the IOMMU is
/// disabled).
pub const IOMMU_CNTL: SmnAddr = 0x13B1_0034;
pub const NB_BUS_NUM_CNTL: SmnAddr = 0x13B1_0044;
/// Naples: MMIO base above DRAM in 64 KiB units.
pub const NB_MMIOBASE: SmnAddr = 0x13B1_005C;
/// Naples: MMIO limit.
pub const NB_MMIOLIMIT: SmnAddr = 0x13B1_0060;
pub const DRAM_TOM2_LOW: SmnAddr = 0x13B1_0064;
pub const DRAM_TOM2_HI: SmnAddr = 0x13B1_0068;
pub const SB_LOCATION: SmnAddr = 0x13B1_007C;
pub const IOHC_FCTL: SmnAddr = 0x13B1_0118;
pub const DRAM_TOM3: SmnAddr = 0x13B1_0138;
pub const PSP_ADDR_LO: SmnAddr = 0x13B1_02E0;
pub const PSP_ADDR_HI: SmnAddr = 0x13B1_02E4;
pub const SMU_ADDR_LO: SmnAddr = 0x13B1_02E8;
pub const SMU_ADDR_HI: SmnAddr = 0x13B1_02EC;
pub const IOAPIC_ADDR_LO: SmnAddr = 0x13B1_02F0;
pub const IOAPIC_ADDR_HI: SmnAddr = 0x13B1_02F4;

/// D0F0x90 `NB_TOP_OF_DRAM_SLOT1` (PCI config of the root bus host bridge).
pub const TOP_OF_DRAM_SLOT1: u16 = 0x90;

register_bitfields![u32,
    pub NB_PCI_CTRL_R [
        /// Set after TOM programming (NBIO_SMU §3.5; unnamed).
        BIT23 OFFSET(23) NUMBITS(1) []
    ],
    pub TOP_OF_DRAM [
        TOM OFFSET(23) NUMBITS(9) [],
        /// TOM is exactly 4 GiB.
        BIT32 OFFSET(0) NUMBITS(1) []
    ],
    pub IOMMU_CNTL_R [
        IOMMU_EN OFFSET(0) NUMBITS(1) []
    ],
    pub BUS_NUM_CNTL [
        EN OFFSET(8) NUMBITS(1) [],
        BUS OFFSET(0) NUMBITS(8) []
    ],
    pub TOM2_LOW [
        TOM2 OFFSET(23) NUMBITS(9) [],
        EN OFFSET(0) NUMBITS(1) []
    ],
    pub TOM2_HI [
        TOM2 OFFSET(0) NUMBITS(9) []
    ],
    pub SB_LOC [
        CORE OFFSET(16) NUMBITS(16) [],
        PORT OFFSET(0) NUMBITS(16) []
    ],
    pub FCTL [
        /// Hot-plug device ID enable.
        HP_DEVID_EN OFFSET(0) NUMBITS(1) [],
        P2P OFFSET(1) NUMBITS(2) [
            ForwardNonMatching = 1,
            ForwardAll = 2
        ],
        ARI OFFSET(22) NUMBITS(1) []
    ],
    pub TOM3 [
        EN OFFSET(31) NUMBITS(1) [],
        /// Limit in 4 MiB units.
        LIMIT OFFSET(0) NUMBITS(30) []
    ],
    pub PSP_LO [
        ADDR OFFSET(20) NUMBITS(12) [],
        LOCK OFFSET(8) NUMBITS(1) [],
        EN OFFSET(0) NUMBITS(1) []
    ],
    pub SMU_LO [
        ADDR OFFSET(20) NUMBITS(12) [],
        LOCK OFFSET(1) NUMBITS(1) [],
        EN OFFSET(0) NUMBITS(1) []
    ],
    pub IOAPIC_LO [
        ADDR OFFSET(8) NUMBITS(24) [],
        LOCK OFFSET(1) NUMBITS(1) [],
        EN OFFSET(0) NUMBITS(1) []
    ],
    pub ADDR_HI [
        ADDR OFFSET(0) NUMBITS(16) []
    ]
];

/// Size of the SMU BAR (FabricAllocateMmio request, NBIO_SMU §3.5).
pub const SMU_BAR_SIZE: u64 = 1 << 20;
/// Size of the PSP BAR (PSP_APOB §3.2).
pub const PSP_BAR_SIZE: u64 = 1 << 20;

/// DRAM top below 4 GiB and above (MSR `TOP_MEM`, `TOM2`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DramTops {
    pub tom: u64,
    /// `None` when SYSCFG.MtrrTom2En is clear.
    pub tom2: Option<u64>,
}

/// AGESA's "TOM2 above 1 TiB" test: MSR TOM2[47:32] > 0x100.
const fn tom2_above_1t(tom2: u64) -> bool {
    tom2 >> 32 > 0x100
}

impl<P: ConfigRegionAccess + Clone> Nbio<P> {
    /// Program TOM, TOM2 and TOM3 (and Naples' MMIO base/limit) on this NBIO
    /// (`GnbSetTom{ZP,SSP,GN}`, NBIO_SMU §3.5, ../sp4r2 NBIO_BASE_ZP §3.5).
    /// AGESA does this on every NBIO without an S3 script.
    pub fn set_tom(&self, tops: DramTops) {
        let naples = self.generation == Generation::Naples;
        // SAFETY: D0F0x90 of this NBIO's host bridge (constructor contract).
        let slot1 = unsafe {
            ConfigRegister::<_, TOP_OF_DRAM::Register>::new(
                self.pci(),
                self.function(0, 0),
                TOP_OF_DRAM_SLOT1,
            )
        };
        let tom = TOP_OF_DRAM::TOM.val((tops.tom >> 23) as u32 & 0x1FF);
        if naples {
            slot1.modify(tom);
        } else {
            slot1.modify(tom + TOP_OF_DRAM::BIT32.val((tops.tom == 1 << 32) as u32));
        }

        let mut mmio_base = 0x0100_0000; // 1 TiB in 64 KiB units
        if let Some(tom2) = tops.tom2 {
            let low = self.reg::<TOM2_LOW::Register>(self.at(DRAM_TOM2_LOW));
            let hi = self.reg::<TOM2_HI::Register>(self.at(DRAM_TOM2_HI));
            if tom2_above_1t(tom2) {
                // The IOHC TOM2 window saturates; DRAM above the
                // 0xFD_0000_0000 hole is decoded by TOM3 (SSP/GN) or the
                // MMIO base (ZP).
                low.modify(TOM2_LOW::TOM2.val(0x1FF) + TOM2_LOW::EN::SET);
                hi.modify(TOM2_HI::TOM2.val(0x1FF));
                if naples {
                    mmio_base = (((tom2 - 1) >> 16) + 1) as u32;
                } else {
                    self.reg::<TOM3::Register>(self.at(DRAM_TOM3)).write(
                        TOM3::EN::SET + TOM3::LIMIT.val(((tom2 - 1) >> 22) as u32 & 0x3FFF_FFFF),
                    );
                }
            } else {
                low.modify(TOM2_LOW::TOM2.val((tom2 >> 23) as u32 & 0x1FF) + TOM2_LOW::EN::SET);
                hi.modify(TOM2_HI::TOM2.val((tom2 >> 32) as u32 & 0x1FF));
            }
        }
        if naples {
            // Fixed ZP values; the donor ORs bit 30 from an uninitialised
            // stack word (hazard), 0x83FFFFFF is the intended value.
            self.write32(DRAM_TOM3, 0x83FF_FFFF);
            self.write32(NB_MMIOBASE, mmio_base);
            self.write32(NB_MMIOLIMIT, 0x7FFF_FFFF);
        }
        self.reg::<NB_PCI_CTRL_R::Register>(self.at(NB_PCI_CTRL))
            .modify(NB_PCI_CTRL_R::BIT23::SET);
    }

    /// `NB_BUS_NUM_CNTL = EN | bus` (NBIO_SMU §3.5 IOAPIC step).
    pub fn set_bus_number(&self) {
        self.reg::<BUS_NUM_CNTL::Register>(self.at(NB_BUS_NUM_CNTL))
            .write(BUS_NUM_CNTL::EN::SET + BUS_NUM_CNTL::BUS.val(self.bus() as u32));
    }

    /// Clear the IOHC IOMMU enable (IOMMU disabled by policy).
    pub fn disable_iommu(&self) {
        self.reg::<IOMMU_CNTL_R::Register>(self.at(IOMMU_CNTL))
            .modify(IOMMU_CNTL_R::IOMMU_EN::CLEAR);
    }

    fn bar(&self, lo: SmnAddr, hi: SmnAddr, en_mask: u32, addr_mask: u32) -> Option<u64> {
        let lo = self.read32(self.at(lo))?;
        let hi = self.read32(self.at(hi))?;
        (lo & en_mask != 0).then_some(((hi as u64 & 0xFFFF) << 32) | (lo & addr_mask) as u64)
    }

    /// The SMU BAR, if enabled.
    pub fn smu_bar(&self) -> Option<u64> {
        self.bar(SMU_ADDR_LO, SMU_ADDR_HI, 1, 0xFFF0_0000)
    }

    /// Place the SMU BAR at `base` (1 MiB aligned, from the NBIO's non-PCI
    /// MMIO window) unless the BAR already holds an address; then enable and
    /// lock it. Returns whether it was written. Rome/Milan do this on every
    /// NBIO; Naples never does (NBIO_SMU §3.5, `nbio.smu_bar`).
    pub fn set_smu_bar(&self, base: u64) -> bool {
        debug_assert_eq!(base % SMU_BAR_SIZE, 0);
        let lo = self.reg::<SMU_LO::Register>(self.at(SMU_ADDR_LO));
        let hi = self.reg::<ADDR_HI::Register>(self.at(SMU_ADDR_HI));
        if lo.get() != 0 || hi.get() != 0 {
            return false;
        }
        hi.write(ADDR_HI::ADDR.val((base >> 32) as u32));
        lo.write(SMU_LO::ADDR.val((base >> 20) as u32 & 0xFFF));
        lo.modify(SMU_LO::EN::SET);
        lo.modify(SMU_LO::LOCK::SET);
        true
    }

    /// The PSP BAR, if enabled.
    pub fn psp_bar(&self) -> Option<u64> {
        self.bar(PSP_ADDR_LO, PSP_ADDR_HI, 1, 0xFFF0_0000)
    }

    /// Place the PSP BAR (1 MiB) at `base` with enable and lock, unless it
    /// is already enabled. Rome/Milan: on IOHC0 of each socket; Naples: on
    /// each die (die 0 of socket 0 in PEI, PSP_APOB §3.2). Returns whether
    /// it was written. The caller also loads `PSP_ADDR` (MSR C001_10A2) on
    /// every thread of the socket/die.
    pub fn set_psp_bar(&self, base: u64) -> bool {
        debug_assert_eq!(base % PSP_BAR_SIZE, 0);
        let lo = self.reg::<PSP_LO::Register>(self.at(PSP_ADDR_LO));
        if lo.is_set(PSP_LO::EN) {
            return false;
        }
        self.reg::<ADDR_HI::Register>(self.at(PSP_ADDR_HI))
            .write(ADDR_HI::ADDR.val((base >> 32) as u32));
        lo.write(
            PSP_LO::ADDR.val((base >> 20) as u32 & 0xFFF) + PSP_LO::LOCK::SET + PSP_LO::EN::SET,
        );
        true
    }

    /// The NBIO IOAPIC base, if enabled.
    pub fn ioapic_bar(&self) -> Option<u64> {
        self.bar(IOAPIC_ADDR_LO, IOAPIC_ADDR_HI, 1, 0xFFFF_FF00)
    }

    /// Place the NBIO IOAPIC at `base` (256-byte aligned, inside the root
    /// bridge's MMIO window): high, low, then enable — the AGESA PCD order
    /// (`gn.ioapic_bar_pcd`).
    pub fn set_ioapic_bar(&self, base: u64) {
        let lo = self.reg::<IOAPIC_LO::Register>(self.at(IOAPIC_ADDR_LO));
        self.reg::<ADDR_HI::Register>(self.at(IOAPIC_ADDR_HI))
            .write(ADDR_HI::ADDR.val((base >> 32) as u32));
        lo.write(IOAPIC_LO::ADDR.val((base >> 8) as u32 & 0xFF_FFFF));
        lo.modify(IOAPIC_LO::EN::SET);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nbio::tests::nbio;

    fn smn(model: &crate::smn::model::Model, bus: u8, addr: SmnAddr) -> u32 {
        model.0.borrow().smn.get(&(bus, addr)).copied().unwrap_or(0)
    }

    #[test]
    fn tom_sp3_below_and_above_1t() {
        let (m, n) = nbio(Generation::Milan, 1, 0x20);
        n.set_tom(DramTops {
            tom: 0x8000_0000,
            tom2: Some(0x80_5000_0000),
        });
        assert_eq!(smn(&m, 0x20, n.at(DRAM_TOM2_LOW)), 0x5000_0001);
        assert_eq!(smn(&m, 0x20, n.at(DRAM_TOM2_HI)), 0x80);
        assert_eq!(smn(&m, 0x20, n.at(DRAM_TOM3)), 0);
        assert_eq!(smn(&m, 0x20, n.at(NB_PCI_CTRL)), 1 << 23);

        let (m, n) = nbio(Generation::Rome, 0, 0);
        m.0.borrow_mut().config.insert((0, 0, 0, 0x90), 0);
        n.set_tom(DramTops {
            tom: 1 << 32,
            tom2: Some(0x200_0000_0000),
        });
        assert_eq!(
            m.0.borrow().config[&(0, 0, 0, 0x90)],
            1,
            "4 GiB: TOM 0, bit 32"
        );
        assert_eq!(smn(&m, 0, DRAM_TOM2_LOW), 0xFF80_0001);
        assert_eq!(smn(&m, 0, DRAM_TOM2_HI), 0x1FF);
        assert_eq!(smn(&m, 0, DRAM_TOM3), 0x8000_0000 | 0x7_FFFF);
    }

    #[test]
    fn tom_naples() {
        let (m, n) = nbio(Generation::Naples, 2, 0x30);
        m.0.borrow_mut().config.insert((0x30, 0, 0, 0x90), 0);
        n.set_tom(DramTops {
            tom: 0xC000_0000,
            tom2: Some(0x2_4000_0000),
        });
        assert_eq!(m.0.borrow().config[&(0x30, 0, 0, 0x90)], 0xC000_0000);
        assert_eq!(smn(&m, 0x30, DRAM_TOM3), 0x83FF_FFFF);
        assert_eq!(smn(&m, 0x30, NB_MMIOBASE), 0x0100_0000);
        assert_eq!(smn(&m, 0x30, DRAM_TOM2_HI), 2);
    }

    #[test]
    fn bars() {
        let (m, n) = nbio(Generation::Rome, 2, 0x40);
        assert!(n.set_smu_bar(0x1_2340_0000));
        assert!(!n.set_smu_bar(0x1_2350_0000), "kept when set");
        assert_eq!(smn(&m, 0x40, n.at(SMU_ADDR_LO)), 0x2340_0003);
        assert_eq!(n.smu_bar(), Some(0x1_2340_0000));
        assert!(n.set_psp_bar(0xF100_0000));
        assert_eq!(smn(&m, 0x40, n.at(PSP_ADDR_LO)), 0xF100_0101);
        n.set_ioapic_bar(0xC000_1000);
        assert_eq!(n.ioapic_bar(), Some(0xC000_1000));
    }
}
