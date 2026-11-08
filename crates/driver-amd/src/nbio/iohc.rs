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
    pub NbPciCtrl [
        USB_PME_DISABLE OFFSET(4) NUMBITS(1) [],
        MMIO_ENABLE OFFSET(23) NUMBITS(1) []
    ],
    /// CFG_IOHC_PCI.
    pub CfgIohcPci [
        /// Config accesses to D0F2, the IOMMU.
        D0F2_ENABLE OFFSET(0) NUMBITS(1) [],
        /// Fused IOMMU disable.
        IOMMU_DIS OFFSET(31) NUMBITS(1) []
    ],
    /// Hardware PCI configuration-retry delay and finite retry count.
    pub ConfigRetry [
        DELAY OFFSET(0) NUMBITS(16) [],
        LIMIT OFFSET(16) NUMBITS(12) []
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
    pub IommuBar [
        ENABLE OFFSET(0) NUMBITS(1) [],
        BASE OFFSET(19) NUMBITS(13) []
    ],
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
pub const CONFIG_RETRY: SmnReg<ConfigRetry::Register> = SmnReg::new(IOHC_BASE + 0x028);
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

/// Publish this IOHC's root-bus identity. Rs NFX AmdNbioBaseInit writes
/// 13B10044h + physical IOS * 1 MiB before link initialization. Preserve
/// reserved bits rather than reproducing its uninitialized stack padding.
/// The caller supplies a checked physical bank, not a filtered root ordinal.
pub fn set_bus_number_at(smn: &impl SmnAccess, offset: u32, bus: u8) -> Result<(), u32> {
    let reg = smn.reg(NB_BUS_NUM_CNTL.offset(offset));
    let mut value = reg.extract();
    if value.get() == u32::MAX {
        return Err(value.get());
    }
    value.modify(NbBusNumCntl::BUS.val(u32::from(bus)) + NbBusNumCntl::LAT_MODE::SET);
    reg.set(value.get());
    let actual = reg.get();
    if actual != value.get() {
        return Err(actual);
    }
    Ok(())
}

/// Publish which IOHC owns the southbridge (FCH) link, at a physical bank.
///
/// Rs NFX AmdNbioBaseInit ffa31ef5..ffa31f7b, next to the bus identity: the
/// FCH's IOHC copies its live SB_LOCATION into the IOAGR L1 and the L2 IOMMU;
/// every other IOHC clears SB_LOCATION. Its reset value claims an SB port,
/// so configuration requests no local function or bridge decodes (absent
/// root-bus devices, unclaimed buses) are forwarded to a link that does not
/// exist and never complete. Returns the location now in force.
pub fn set_southbridge_location_at(
    smn: &impl SmnAccess,
    offset: u32,
    fch: bool,
) -> Result<u32, u32> {
    use super::iommu::{L1_SSP, L2_SB_LOCATION, l1_sb_location};
    let location = smn.reg(SB_LOCATION.offset(offset));
    let value = location.get();
    if value == u32::MAX || (fch && value == 0) {
        return Err(value);
    }
    if !fch {
        location.set(0);
        return match location.get() {
            0 => Ok(0),
            actual => Err(actual),
        };
    }
    for copy in [l1_sb_location(L1_SSP[3]), L2_SB_LOCATION] {
        let copy = smn.reg(copy.offset(offset));
        copy.set(value);
        if copy.get() != value {
            return Err(copy.get());
        }
    }
    Ok(value)
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
        self.map_registers(&smn.reg(self.low), &smn.reg(self.high), base);
    }
    /// The same BAR sequence over typed registers: either indexed SMN or
    /// the block's own PCI capability. Selecting a root's SMN access bus
    /// alone does not relocate an IOD-wide register address.
    pub fn map_registers(
        &self,
        low: &impl Writeable<T = u32, R = R>,
        high: &impl Writeable<T = u32, R = BarHigh::Register>,
        base: u32,
    ) {
        let address = || self.address.val(base >> self.address.shift);
        high.set(0);
        low.write(address());
        low.write(address() + self.enable.val(1));
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
        self.base_registers(&smn.reg(self.low), &smn.reg(self.high))
    }
    /// Decode an enabled BAR from either register transport.
    pub fn base_registers(
        &self,
        low: &impl Readable<T = u32, R = R>,
        high: &impl Readable<T = u32, R = BarHigh::Register>,
    ) -> Option<u64> {
        let low = low.extract();
        low.is_set(self.enable).then(|| {
            u64::from(high.read(BarHigh::ADDRESS)) << 32
                | u64::from(low.read(self.address) << self.address.shift)
        })
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
    set_tom_at(smn, slot1, tops, style, 0)
}

/// The same IOHC register mechanism on an explicitly selected SMN bank.
/// Rs host bridges share the SMN namespace; physical IOHC n is relocated by
/// n*1MiB. Existing translated SMN views continue to use offset zero.
pub fn set_tom_at(
    smn: &impl SmnAccess,
    slot1: &impl ReadWriteable<T = u32, R = TopOfDram::Register>,
    tops: DramTops,
    style: TopStyle,
    offset: u32,
) {
    let tom = TopOfDram::TOM.val((tops.tom >> 23) as u32);
    slot1.modify(match style {
        TopStyle::MmioWindow => tom,
        TopStyle::Tom3 => tom + TopOfDram::TOM_4G.val((tops.tom == 1 << 32) as u32),
    });
    // 1 TiB in 64-KiB units.
    let mut mmio_base = 0x0100_0000;
    if let Some(tom2) = tops.tom2 {
        let lower = smn.reg(NB_LOWER_TOP_OF_DRAM2.offset(offset));
        let upper = smn.reg(NB_UPPER_TOP_OF_DRAM2.offset(offset));
        if tom2_above_1t(tom2) {
            // TOM2 stops at the HyperTransport hole below 1 TiB; DRAM above
            // it is decoded by TOM3 or the MMIO base.
            lower.modify(LowerTopOfDram2::TOM2.val(0x1ff) + LowerTopOfDram2::ENABLE::SET);
            upper.modify(UpperTopOfDram2::TOM2.val(0x1ff));
            match style {
                TopStyle::MmioWindow => mmio_base = (((tom2 - 1) >> 16) + 1) as u32,
                TopStyle::Tom3 => smn.reg(NB_TOP_OF_DRAM3.offset(offset)).write(
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
        smn.reg(NB_TOP_OF_DRAM3.offset(offset)).set(0x83ff_ffff);
        smn.reg(NB_MMIOBASE.offset(offset)).set(mmio_base);
        smn.reg(NB_MMIOLIMIT.offset(offset)).set(0x7fff_ffff);
    }
    smn.reg(NB_PCI_CTRL.offset(offset))
        .modify(NbPciCtrl::MMIO_ENABLE::SET);
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
    fn rs_configuration_retry_fields_preserve_reserved_high_nibble() {
        let hw = SmnSpace::default();
        let reg = CONFIG_RETRY.offset(0x200000);
        assert_eq!(reg.address(), 0x13d1_0028);
        hw.set(reg, 0xa000_0000);
        hw.smn()
            .reg(reg)
            .modify(ConfigRetry::DELAY.val(6) + ConfigRetry::LIMIT.val(0x262));
        assert_eq!(hw.get(reg), 0xa262_0006);
        assert_eq!(hw.get(CONFIG_RETRY), 0);
    }

    #[test]
    fn root_bus_identity_uses_physical_bank_and_preserves_reserved_bits() {
        let space = SmnSpace::default();
        for (bank, bus) in [(0, 0xc0), (0x200000, 0), (0x300000, 0x40)] {
            space.set(NB_BUS_NUM_CNTL.offset(bank), 0xabcd_2000);
            assert_eq!(set_bus_number_at(&space.smn(), bank, bus), Ok(()));
            assert_eq!(
                space.get(NB_BUS_NUM_CNTL.offset(bank)),
                0xabcd_2100 | u32::from(bus)
            );
        }
        assert_eq!(space.get(NB_BUS_NUM_CNTL.offset(0x100000)), 0);
        space.set(NB_BUS_NUM_CNTL, u32::MAX);
        assert_eq!(set_bus_number_at(&space.smn(), 0, 0), Err(u32::MAX));
        assert_eq!(space.get(NB_BUS_NUM_CNTL), u32::MAX);
    }

    #[test]
    fn southbridge_location_is_forwarded_by_the_fch_iohc_and_cleared_elsewhere() {
        use crate::nbio::iommu::{L1_SSP, L2_SB_LOCATION, l1_sb_location};
        let space = SmnSpace::default();
        for bank in [0u32, 0x200000, 0x300000] {
            space.set(SB_LOCATION.offset(bank), 0x0004_0001);
        }
        let smn = space.smn();
        assert_eq!(
            set_southbridge_location_at(&smn, 0x200000, true),
            Ok(0x40001)
        );
        assert_eq!(
            space.get(l1_sb_location(L1_SSP[3]).offset(0x200000)),
            0x40001
        );
        assert_eq!(space.get(L2_SB_LOCATION.offset(0x200000)), 0x40001);
        assert_eq!(space.get(SB_LOCATION.offset(0x200000)), 0x40001);
        for bank in [0u32, 0x300000] {
            assert_eq!(set_southbridge_location_at(&smn, bank, false), Ok(0));
            assert_eq!(space.get(SB_LOCATION.offset(bank)), 0);
            assert_eq!(space.get(L2_SB_LOCATION.offset(bank)), 0);
        }
        // An FCH IOHC without a location, or an absent bank, is rejected
        // before any copy is written.
        space.set(SB_LOCATION.offset(0x100000), 0);
        assert_eq!(set_southbridge_location_at(&smn, 0x100000, true), Err(0));
        assert_eq!(space.get(L2_SB_LOCATION.offset(0x100000)), 0);
        space.set(SB_LOCATION.offset(0x100000), u32::MAX);
        assert_eq!(
            set_southbridge_location_at(&smn, 0x100000, false),
            Err(u32::MAX)
        );
        assert_eq!(space.get(SB_LOCATION.offset(0x100000)), u32::MAX);
    }

    #[test]
    fn explicit_physical_iohc_bank_does_not_touch_bank_zero() {
        let space = SmnSpace::default();
        let slot = tock_registers::registers::InMemoryRegister::new(0);
        set_tom_at(
            &space.smn(),
            &slot,
            DramTops {
                tom: 0xb0000000,
                tom2: Some(0x6050000000),
            },
            TopStyle::Tom3,
            0x200000,
        );
        assert_eq!(slot.get(), 0xb0000000);
        assert_eq!(
            space.get(NB_LOWER_TOP_OF_DRAM2.offset(0x200000)),
            0x50000001
        );
        assert_eq!(space.get(NB_UPPER_TOP_OF_DRAM2.offset(0x200000)), 0x60);
        assert_eq!(space.get(NB_PCI_CTRL.offset(0x200000)), 1 << 23);
        assert_eq!(space.get(NB_LOWER_TOP_OF_DRAM2), 0);
        assert_eq!(space.get(NB_PCI_CTRL), 0);
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
    fn per_function_pci_capability_bars_do_not_alias_other_roots() {
        use fstart_pci::{ConfigRegister, PciAddress, PciEcam, PciRoot, PciRootInfo};
        let mut memory = std::vec![0u32; 4 << 18];
        let info = PciRootInfo {
            segment: 0,
            ecam_base: memory.as_mut_ptr() as u64,
            bus_start: 0,
            bus_end: 3,
        };
        let pci = PciEcam::new([PciRoot::new(info, []).unwrap()]).unwrap();
        for bus in 0..4u8 {
            let address = PciAddress::new(0, bus, 0, 2);
            // SAFETY: exclusively owned modeled ECAM, aligned dword fields.
            let (low, high) = unsafe {
                (
                    ConfigRegister::new(&pci, address, super::super::iommu::CAPABILITY + 4),
                    ConfigRegister::new(&pci, address, super::super::iommu::CAPABILITY + 8),
                )
            };
            let base = 0xb6000000 + u32::from(bus) * 0x100000;
            assert_eq!(IOMMU_BAR.base_registers(&low, &high), None);
            IOMMU_BAR.map_registers(&low, &high, base);
            assert_eq!(IOMMU_BAR.base_registers(&low, &high), Some(u64::from(base)));
            for earlier in 0..=bus {
                let dev = pci.device(PciAddress::new(0, earlier, 0, 2)).unwrap();
                assert_eq!(
                    dev.read32(super::super::iommu::CAPABILITY + 4),
                    0xb6000001 + u32::from(earlier) * 0x100000
                );
                assert_eq!(dev.read32(super::super::iommu::CAPABILITY + 8), 0);
            }
        }
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
