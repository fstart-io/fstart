// SPDX-License-Identifier: MIT OR Apache-2.0
//! The IOHC of a die (ZP: Zeppelin, Naples; RV: Picasso) or IOD quadrant
//! (SSP: Rome; GN: Milan): the DRAM map it routes by, the MMIO BARs of the
//! NBIO blocks behind it (PSP, SMU, IOAPIC, IOMMU) and its root complex
//! registers, all in SMN, at the same addresses on every Family 17h/19h
//! SoC (names as in the PPR). The addresses are those of IOHC0 / die 0; an
//! SP3 NBIO quadrant relocates them through its [`SmnAccess`].
use crate::smn::{SmnAccess, SmnReg};
use tock_registers::{
    RegisterLongName,
    fields::Field,
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};
#[cfg(any(feature = "picasso", feature = "zeppelin"))]
use {
    crate::{df::Die, soc::Soc},
    fstart_pci::ConfigRegionAccess,
};

register_bitfields![u32,
    /// NB_TOP_OF_DRAM_SLOT1 (D0F0x90).
    pub TopOfDram [
        /// TOM is exactly 4 GiB (SSP, GN).
        TOM_4G OFFSET(0) NUMBITS(1) [],
        TOM OFFSET(23) NUMBITS(9) []
    ],
    pub NbPciCtrl [MMIO_ENABLE OFFSET(23) NUMBITS(1) []],
    /// CFG_IOHC_PCI.
    pub CfgIohcPci [
        /// Config accesses to D0F2, the IOMMU.
        D0F2_ENABLE OFFSET(0) NUMBITS(1) [],
        /// Fused IOMMU disable.
        IOMMU_DIS OFFSET(31) NUMBITS(1) []
    ],
    pub NbBusNumCntl [
        BUS OFFSET(0) NUMBITS(8) [],
        LAT_MODE OFFSET(8) NUMBITS(1) []
    ],
    pub LowerTopOfDram2 [
        ENABLE OFFSET(0) NUMBITS(1) [],
        /// TOM2[31:23].
        TOM2 OFFSET(23) NUMBITS(9) []
    ],
    /// TOM2[40:32].
    pub UpperTopOfDram2 [TOM2 OFFSET(0) NUMBITS(9) []],
    pub SbLocation [
        PORT OFFSET(0) NUMBITS(16) [],
        CORE OFFSET(16) NUMBITS(16) []
    ],
    pub IohcFeatureCntl [
        /// Hot-plug message device ID enable.
        HP_DEVID_EN OFFSET(0) NUMBITS(1) [],
        P2P OFFSET(1) NUMBITS(2) [
            DropNonMatching = 0,
            ForwardNonMatching = 1,
            ForwardAll = 2,
            Disabled = 3
        ],
        /// ARI support (vendor PCD 0x1d).
        ARI OFFSET(22) NUMBITS(1) []
    ],
    pub TopOfDram3 [
        /// Limit in 4 MiB units.
        LIMIT OFFSET(0) NUMBITS(30) [],
        ENABLE OFFSET(31) NUMBITS(1) []
    ],
    pub NbProgDeviceRemap [DEVFN OFFSET(0) NUMBITS(8) []],
    pub IohcBridgeCntl [
        BRIDGE_DIS OFFSET(0) NUMBITS(1) [],
        CFG_DIS OFFSET(2) NUMBITS(1) [],
        CRS_ENABLE OFFSET(18) NUMBITS(1) []
    ],
    pub IohcRbBusRoute [
        BASE OFFSET(0) NUMBITS(8) [],
        LIMIT OFFSET(8) NUMBITS(8) [],
        /// The range is another root bridge's.
        REMOTE OFFSET(31) NUMBITS(1) []
    ],
    /// PSP_BASE_ADDR_LO.
    pub PspBar [
        ENABLE OFFSET(0) NUMBITS(1) [],
        LOCK OFFSET(8) NUMBITS(1) [],
        /// Address bits [31:20].
        BASE OFFSET(20) NUMBITS(12) []
    ],
    /// SMU_BASE_ADDR_LO.
    pub SmuBar [
        ENABLE OFFSET(0) NUMBITS(1) [],
        LOCK OFFSET(1) NUMBITS(1) [],
        /// Address bits [31:20].
        BASE OFFSET(20) NUMBITS(12) []
    ],
    /// IOAPIC_BASE_ADDR_LO.
    pub IoapicBar [
        ENABLE OFFSET(0) NUMBITS(1) [],
        LOCK OFFSET(1) NUMBITS(1) [],
        ADDRESS OFFSET(8) NUMBITS(24) []
    ],
    /// The IOMMU capability's base address register; the base is 512 KiB
    /// aligned.
    pub IommuBar [ENABLE OFFSET(0) NUMBITS(1) [], BASE OFFSET(19) NUMBITS(13) []],
    /// Bits 47:32 of a BAR.
    pub BarHigh [ADDRESS OFFSET(0) NUMBITS(16) []]
];
// NB configuration space (SMN), also D0F0 of the root bus.
pub const NB_PCI_CTRL: SmnReg<NbPciCtrl::Register> = SmnReg::new(0x13b0_004c);
/// Subsystem IDs (D0F0x50 alias).
pub const NB_ADAPTER_ID_W: SmnReg = SmnReg::new(0x13b0_0050);
/// NB_TOP_OF_DRAM_SLOT1, in the root bus's D0F0 config space.
pub const NB_TOP_OF_DRAM_SLOT1: u16 = 0x90;
const IOHC_BASE: u32 = 0x13b1_0000;
pub const CFG_IOHC_PCI: SmnReg<CfgIohcPci::Register> = SmnReg::new(IOHC_BASE + 0x034);
pub const NB_BUS_NUM_CNTL: SmnReg<NbBusNumCntl::Register> = SmnReg::new(IOHC_BASE + 0x044);
/// MMIO base above DRAM, address bits 47:16.
pub const NB_MMIOBASE: SmnReg = SmnReg::new(IOHC_BASE + 0x05c);
pub const NB_MMIOLIMIT: SmnReg = SmnReg::new(IOHC_BASE + 0x060);
pub const NB_LOWER_TOP_OF_DRAM2: SmnReg<LowerTopOfDram2::Register> = SmnReg::new(IOHC_BASE + 0x064);
pub const NB_UPPER_TOP_OF_DRAM2: SmnReg<UpperTopOfDram2::Register> = SmnReg::new(IOHC_BASE + 0x068);
pub const SB_LOCATION: SmnReg<SbLocation::Register> = SmnReg::new(IOHC_BASE + 0x07c);
pub const fn nb_prog_device_remap(port: usize) -> SmnReg<NbProgDeviceRemap::Register> {
    SmnReg::new(IOHC_BASE + 0x0c0 + port as u32 * 4)
}
pub const IOHC_FEATURE_CNTL: SmnReg<IohcFeatureCntl::Register> = SmnReg::new(IOHC_BASE + 0x118);
pub const NB_TOP_OF_DRAM3: SmnReg<TopOfDram3::Register> = SmnReg::new(IOHC_BASE + 0x138);
/// Bus routing between the root bridges: entry `rb` of the system's.
pub const fn iohc_rb_bus_route(rb: usize) -> SmnReg<IohcRbBusRoute::Register> {
    SmnReg::new(IOHC_BASE + 0x190 + rb as u32 * 0x14)
}
/// The 16 PCIe bridges of a die each have a bridge control register.
pub const fn iohc_bridge_cntl(bridge: usize) -> SmnReg<IohcBridgeCntl::Register> {
    SmnReg::new(0x13b3_1004 + bridge as u32 * 0x400)
}

/// A 64-bit MMIO BAR in SMN: the low half holds the enable bit and the
/// address bits of `address`, the next register address bits 47:32.
pub struct Bar<R: RegisterLongName> {
    pub low: SmnReg<R>,
    pub high: SmnReg<BarHigh::Register>,
    address: Field<u32, R>,
    enable: Field<u32, R>,
}
impl<R: RegisterLongName> Bar<R> {
    pub const fn new(low: u32, address: Field<u32, R>, enable: Field<u32, R>) -> Self {
        Self {
            low: SmnReg::new(low),
            high: SmnReg::new(low + 4),
            address,
            enable,
        }
    }
    /// Decode the block at `base` below 4 GiB: clear the high half, write
    /// the address, then enable it.
    pub fn map(&self, smn: &impl SmnAccess, base: u32) {
        let address = || self.address.val(base >> self.address.shift);
        smn.reg(self.high).set(0);
        smn.reg(self.low).write(address());
        smn.reg(self.low).write(address() + self.enable.val(1));
    }
    /// The low half of the decoded base, `None` if the BAR isn't enabled.
    /// [`Self::map`] clears the high half.
    pub fn base_below_4g(&self, smn: &impl SmnAccess) -> Option<u32> {
        let low = smn.reg(self.low).extract();
        low.is_set(self.enable)
            .then(|| low.read(self.address) << self.address.shift)
    }
    /// The decoded base, `None` if the BAR isn't enabled. Reads the high
    /// half only for an enabled BAR.
    pub fn base(&self, smn: &impl SmnAccess) -> Option<u64> {
        let low = self.base_below_4g(smn)?;
        Some(u64::from(smn.reg(self.high).read(BarHigh::ADDRESS)) << 32 | u64::from(low))
    }
}

pub const PSP_BAR: Bar<PspBar::Register> =
    Bar::new(IOHC_BASE + 0x2e0, PspBar::BASE, PspBar::ENABLE);
pub const SMU_BAR: Bar<SmuBar::Register> =
    Bar::new(IOHC_BASE + 0x2e8, SmuBar::BASE, SmuBar::ENABLE);
pub const IOAPIC_BAR: Bar<IoapicBar::Register> =
    Bar::new(IOHC_BASE + 0x2f0, IoapicBar::ADDRESS, IoapicBar::ENABLE);
/// In the IOMMU's (D0F2) config space alias, its capability at 0x40.
pub const IOMMU_BAR: Bar<IommuBar::Register> =
    Bar::new(0x13f0_0044, IommuBar::BASE, IommuBar::ENABLE);
/// The PSP and SMU windows are 1 MiB.
pub const PSP_BAR_SIZE: u64 = 1 << 20;
pub const SMU_BAR_SIZE: u64 = 1 << 20;

/// The low half [`map_psp`] programs for `base`.
pub fn psp_bar(base: u32) -> tock_registers::fields::FieldValue<u32, PspBar::Register> {
    PspBar::BASE.val(base >> 20) + PspBar::LOCK::SET + PspBar::ENABLE::SET
}

/// Map the PSP's 1 MiB register window at `base` below 4 GiB, enabled and
/// locked, unless the low half already is. A locked BAR at another address
/// doesn't take the write: `Err` with its low half.
pub fn map_psp(smn: &impl SmnAccess, base: u32) -> Result<(), u32> {
    let bar = smn.reg(PSP_BAR.low).extract();
    let want = psp_bar(base);
    if want.matches_all(bar.get()) {
        return Ok(());
    }
    smn.reg(PSP_BAR.high).set(0);
    smn.reg(PSP_BAR.low).write(want);
    if bar.is_set(PspBar::LOCK) {
        Err(bar.get())
    } else {
        Ok(())
    }
}

/// The DRAM tops: MSR TOP_MEM and, when SYSCFG enables it, TOM2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DramTops {
    pub tom: u64,
    pub tom2: Option<u64>,
}

/// How an IOHC routes DRAM above TOM2's reach (1 TiB): with fixed TOM3
/// and an MMIO window above TOM2 (ZP, RV), or with TOM3 (SSP, GN).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopStyle {
    MmioWindow,
    Tom3,
}

/// AGESA's "TOM2 above 1 TiB" test: TOM2[47:32] > 0x100.
const fn tom2_above_1t(tom2: u64) -> bool {
    tom2 >> 32 > 0x100
}

/// Tell an IOHC about the DRAM map and the MMIO above it
/// (`GnbSetTom{ZP,RV,SSP,GN}`): TOM, TOM2 and TOM3 or the MMIO window.
/// `slot1` is [`NB_TOP_OF_DRAM_SLOT1`] of the IOHC's host bridge.
pub fn set_tom(
    smn: &impl SmnAccess,
    slot1: &impl ReadWriteable<T = u32, R = TopOfDram::Register>,
    tops: DramTops,
    style: TopStyle,
) {
    let tom = TopOfDram::TOM.val((tops.tom >> 23) as u32);
    slot1.modify(match style {
        TopStyle::MmioWindow => tom,
        TopStyle::Tom3 => tom + TopOfDram::TOM_4G.val((tops.tom == 1 << 32) as u32),
    });
    // 1 TiB in 64-KiB units.
    let mut mmio_base = 0x0100_0000;
    if let Some(tom2) = tops.tom2 {
        let lower = smn.reg(NB_LOWER_TOP_OF_DRAM2);
        let upper = smn.reg(NB_UPPER_TOP_OF_DRAM2);
        if tom2_above_1t(tom2) {
            // TOM2 stops at the HyperTransport hole below 1 TiB; DRAM above
            // it is decoded by TOM3 or the MMIO base.
            lower.modify(LowerTopOfDram2::TOM2.val(0x1ff) + LowerTopOfDram2::ENABLE::SET);
            upper.modify(UpperTopOfDram2::TOM2.val(0x1ff));
            match style {
                TopStyle::MmioWindow => mmio_base = (((tom2 - 1) >> 16) + 1) as u32,
                TopStyle::Tom3 => smn.reg(NB_TOP_OF_DRAM3).write(
                    TopOfDram3::ENABLE::SET + TopOfDram3::LIMIT.val(((tom2 - 1) >> 22) as u32),
                ),
            }
        } else {
            lower.modify(
                LowerTopOfDram2::TOM2.val((tom2 >> 23) as u32) + LowerTopOfDram2::ENABLE::SET,
            );
            upper.modify(UpperTopOfDram2::TOM2.val((tom2 >> 32) as u32));
        }
    }
    if style == TopStyle::MmioWindow {
        // The fixed ZP values; the donor ORs bit 30 from an uninitialised
        // stack word (hazard), 0x83ffffff is the intended value.
        smn.reg(NB_TOP_OF_DRAM3).set(0x83ff_ffff);
        smn.reg(NB_MMIOBASE).set(mmio_base);
        smn.reg(NB_MMIOLIMIT).set(0x7fff_ffff);
    }
    smn.reg(NB_PCI_CTRL).modify(NbPciCtrl::MMIO_ENABLE::SET);
}

/// Tell the IOHC of a die about the DRAM map and the MMIO above 4 GiB.
/// `top_mem` is TOP_MEM (MSR C001_001A), `top_mem2` TOP_MEM2 if SYSCFG
/// enables it.
#[cfg(any(feature = "picasso", feature = "zeppelin"))]
pub fn dram_map<S: Soc, P: ConfigRegionAccess + Copy>(
    die: &Die<S, P>,
    top_mem: u32,
    top_mem2: Option<u64>,
) {
    // A zero TOM2 leaves both TOM2 registers alone, as AGESA does.
    let tops = DramTops {
        tom: top_mem.into(),
        tom2: top_mem2.filter(|&tom2| tom2 != 0),
    };
    let root = fstart_pci::PciAddress::new(0, die.bus(), 0, 0);
    // SAFETY: D0F0 of the die's root bus; exclusivity is the `Die` contract.
    let slot1 = unsafe { fstart_pci::ConfigRegister::new(die.pci(), root, NB_TOP_OF_DRAM_SLOT1) };
    set_tom(&die.smn(), &slot1, tops, TopStyle::MmioWindow);
    fstart_log::debug!(
        "IOHC {}: TOM {} TOM2 {}",
        die.index(),
        fstart_log::Hex(top_mem.into()),
        fstart_log::Hex(top_mem2.unwrap_or(0))
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smn::testing::SmnSpace;

    fn tom(style: TopStyle, tops: DramTops) -> (SmnSpace, u32) {
        let space = SmnSpace::default();
        let slot1 = tock_registers::registers::InMemoryRegister::new(0);
        set_tom(&space.smn(), &slot1, tops, style);
        (space, slot1.get())
    }

    #[test]
    fn tom2_below_and_above_1t_with_the_mmio_window() {
        // 32 GiB: TOM2 in the IOHC, MMIO from 1 TiB up.
        let (space, slot1) = tom(
            TopStyle::MmioWindow,
            DramTops {
                tom: 0xc000_0000,
                tom2: Some(0x8_0000_0000),
            },
        );
        assert_eq!(slot1, 0xc000_0000);
        assert_eq!(space.get(NB_LOWER_TOP_OF_DRAM2), 1);
        assert_eq!(space.get(NB_UPPER_TOP_OF_DRAM2), 8);
        assert_eq!(space.get(NB_TOP_OF_DRAM3), 0x83ff_ffff);
        assert_eq!(space.get(NB_MMIOBASE), 0x0100_0000);
        assert_eq!(space.get(NB_MMIOLIMIT), 0x7fff_ffff);
        assert_eq!(space.get(NB_PCI_CTRL), 1 << 23);
        // Exactly 1 TiB still fits TOM2.
        let (space, _slot1) = tom(
            TopStyle::MmioWindow,
            DramTops {
                tom: 0xc000_0000,
                tom2: Some(1 << 40),
            },
        );
        assert_eq!(space.get(NB_UPPER_TOP_OF_DRAM2), 0x100);
        // Above: TOM2 stops at the hole, the MMIO base above TOM2.
        let (space, _slot1) = tom(
            TopStyle::MmioWindow,
            DramTops {
                tom: 0xc000_0000,
                tom2: Some(0x101_0000_0000),
            },
        );
        assert_eq!(space.get(NB_LOWER_TOP_OF_DRAM2), 0xff80_0001);
        assert_eq!(space.get(NB_UPPER_TOP_OF_DRAM2), 0x1ff);
        assert_eq!(space.get(NB_MMIOBASE), 0x0101_0000);
        // No TOM2.
        let (space, _slot1) = tom(
            TopStyle::MmioWindow,
            DramTops {
                tom: 0xc000_0000,
                tom2: None,
            },
        );
        assert_eq!(space.get(NB_LOWER_TOP_OF_DRAM2), 0);
        assert_eq!(space.get(NB_MMIOBASE), 0x0100_0000);
    }

    #[test]
    fn tom2_below_and_above_1t_with_tom3() {
        let (space, _slot1) = tom(
            TopStyle::Tom3,
            DramTops {
                tom: 0x8000_0000,
                tom2: Some(0x80_5000_0000),
            },
        );
        assert_eq!(space.get(NB_LOWER_TOP_OF_DRAM2), 0x5000_0001);
        assert_eq!(space.get(NB_UPPER_TOP_OF_DRAM2), 0x80);
        assert_eq!(space.get(NB_TOP_OF_DRAM3), 0);
        assert_eq!(space.get(NB_MMIOBASE), 0);
        assert_eq!(space.get(NB_PCI_CTRL), 1 << 23);
        let (space, slot1) = tom(
            TopStyle::Tom3,
            DramTops {
                tom: 1 << 32,
                tom2: Some(0x200_0000_0000),
            },
        );
        assert_eq!(slot1, 1, "4 GiB: TOM 0, TOM_4G");
        assert_eq!(space.get(NB_LOWER_TOP_OF_DRAM2), 0xff80_0001);
        assert_eq!(space.get(NB_UPPER_TOP_OF_DRAM2), 0x1ff);
        assert_eq!(space.get(NB_TOP_OF_DRAM3), 0x8000_0000 | 0x7_ffff);
    }

    #[test]
    fn bars_map_high_half_address_then_enable() {
        let space = SmnSpace::default();
        let smn = space.smn();
        space.set(IOMMU_BAR.high, 0xdead);
        assert_eq!(IOMMU_BAR.base(&smn), None);
        IOMMU_BAR.map(&smn, 0xfe60_0000);
        assert_eq!(space.get(IOMMU_BAR.low), 0xfe60_0001);
        assert_eq!(IOMMU_BAR.base(&smn), Some(0xfe60_0000));
        IOAPIC_BAR.map(&smn, 0xfec0_1000);
        assert_eq!(space.get(IOAPIC_BAR.low), 0xfec0_1001);

        // Locked elsewhere: reported; right already: left alone.
        space.set(PSP_BAR.low, 0xd010_0101);
        space.set(PSP_BAR.high, 0xdead);
        assert_eq!(map_psp(&smn, 0xd010_0000), Ok(()));
        assert_eq!(space.get(PSP_BAR.high), 0xdead);
        assert_eq!(map_psp(&smn, 0xfd10_0000), Err(0xd010_0101));
        assert_eq!(space.get(PSP_BAR.low), 0xfd10_0101);
        assert_eq!(PSP_BAR.base_below_4g(&smn), Some(0xfd10_0000));
    }
}
