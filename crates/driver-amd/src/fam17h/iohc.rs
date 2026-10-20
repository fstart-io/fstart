// SPDX-License-Identifier: MIT OR Apache-2.0
//! The IOHC of a die: the DRAM map it routes by and the MMIO BARs of the
//! NBIO blocks behind it (PSP, SMU, IOAPIC, IOMMU), all in SMN. The same IP
//! at the same addresses on Raven/Picasso and Zeppelin; the SoC picks the
//! base addresses.
use super::topology::{Die, Soc};
use crate::smn::{Smn, SmnReg};
use fstart_pci::{ConfigRegionAccess, ConfigRegister, PciAddress};
use tock_registers::{
    RegisterLongName,
    fields::Field,
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

register_bitfields![u32,
    /// D0F0x90 (also SMN 0x13b00090).
    pub TopOfDram [TOM OFFSET(23) NUMBITS(9) []],
    pub NbPciCtrl [MMIO_ENABLE OFFSET(23) NUMBITS(1) []],
    pub IoapicBar [ENABLE OFFSET(0) NUMBITS(1) [], ADDRESS OFFSET(8) NUMBITS(24) []],
    /// The PSP and SMU BARs (low halves).
    pub LockedBar [
        ENABLE OFFSET(0) NUMBITS(1) [],
        LOCK OFFSET(8) NUMBITS(1) [],
        /// Address bits [31:20].
        BASE OFFSET(20) NUMBITS(12) []
    ],
    /// The IOMMU capability's base address register; the base is 512 KiB
    /// aligned.
    pub IommuBar [ENABLE OFFSET(0) NUMBITS(1) [], BASE OFFSET(19) NUMBITS(13) []]
];

// NB configuration space (SMN), also D0F0 of the die.
pub const NB_PCI_CTRL: SmnReg<NbPciCtrl::Register> = SmnReg::new(0x13b0_004c);
/// D0F0x90, in config space.
pub const NB_TOP_OF_DRAM_SLOT1: u16 = 0x90;

const IOHC_BASE: u32 = 0x13b1_0000;
pub const NB_MMIOBASE: SmnReg = SmnReg::new(IOHC_BASE + 0x05c);
pub const NB_MMIOLIMIT: SmnReg = SmnReg::new(IOHC_BASE + 0x060);
pub const NB_LOWER_TOP_OF_DRAM2: SmnReg = SmnReg::new(IOHC_BASE + 0x064);
pub const NB_UPPER_TOP_OF_DRAM2: SmnReg = SmnReg::new(IOHC_BASE + 0x068);
pub const NB_TOP_OF_DRAM3: SmnReg = SmnReg::new(IOHC_BASE + 0x138);

/// A 64-bit MMIO BAR in SMN: the low half holds the enable bit and the
/// address bits of `address`, the next register address bits 63:32.
pub struct Bar<R: RegisterLongName> {
    pub low: SmnReg<R>,
    pub high: SmnReg,
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
    /// Decode the block at `base`: clear the high half, write the address,
    /// then enable it.
    pub fn map(&self, smn: &Smn<impl ConfigRegionAccess>, base: u32) {
        let address = || self.address.val(base >> self.address.shift);
        smn.reg(self.high).set(0);
        smn.reg(self.low).write(address());
        smn.reg(self.low).write(address() + self.enable.val(1));
    }
    /// The low half of the decoded base, `None` if the BAR isn't enabled.
    /// [`Self::map`] clears the high half.
    pub fn base_below_4g(&self, smn: &Smn<impl ConfigRegionAccess>) -> Option<u32> {
        let low = smn.reg(self.low).extract();
        low.is_set(self.enable)
            .then(|| low.read(self.address) << self.address.shift)
    }
    /// The decoded base, `None` if the BAR isn't enabled. Reads the high
    /// half only for an enabled BAR.
    pub fn base(&self, smn: &Smn<impl ConfigRegionAccess>) -> Option<u64> {
        let low = self.base_below_4g(smn)?;
        Some(u64::from(smn.reg(self.high).get()) << 32 | u64::from(low))
    }
}

pub const PSP_BAR: Bar<LockedBar::Register> =
    Bar::new(IOHC_BASE + 0x2e0, LockedBar::BASE, LockedBar::ENABLE);
pub const SMU_BAR: Bar<LockedBar::Register> =
    Bar::new(IOHC_BASE + 0x2e8, LockedBar::BASE, LockedBar::ENABLE);
pub const IOAPIC_BAR: Bar<IoapicBar::Register> =
    Bar::new(IOHC_BASE + 0x2f0, IoapicBar::ADDRESS, IoapicBar::ENABLE);
/// In the IOMMU's (D0F2) config space alias, its capability at 0x40.
pub const IOMMU_BAR: Bar<IommuBar::Register> =
    Bar::new(0x13f0_0044, IommuBar::BASE, IommuBar::ENABLE);

/// The low half [`map_psp`] programs for `base`.
pub fn psp_bar(base: u32) -> tock_registers::fields::FieldValue<u32, LockedBar::Register> {
    LockedBar::BASE.val(base >> 20) + LockedBar::LOCK::SET + LockedBar::ENABLE::SET
}

/// Map the PSP's 1 MiB register window at `base`, enabled and locked,
/// unless the low half already is. A locked BAR at another address doesn't
/// take the write: `Err` with its low half.
pub fn map_psp(smn: &Smn<impl ConfigRegionAccess>, base: u32) -> Result<(), u32> {
    let bar = smn.reg(PSP_BAR.low).extract();
    let want = psp_bar(base);
    if want.matches_all(bar.get()) {
        return Ok(());
    }
    smn.reg(PSP_BAR.high).set(0);
    smn.reg(PSP_BAR.low).write(want);
    if bar.is_set(LockedBar::LOCK) {
        Err(bar.get())
    } else {
        Ok(())
    }
}

/// IOHC TOM2/MMIO base registers for a TOP_MEM2 (0: disabled): (lower,
/// upper, MMIO base).
fn top_of_dram2(lower: u32, upper: u32, top_mem2: u64) -> (u32, u32, u32) {
    if top_mem2 > 1 << 40 {
        // TOM2 stops at the HyperTransport hole, the rest is above TOM3.
        (
            lower | 0xff80_0001,
            upper | 0x1ff,
            (((top_mem2 - 1) >> 16) + 1) as u32,
        )
    } else if top_mem2 != 0 {
        (
            (lower & 0x007f_fffe) | (top_mem2 as u32 & 0xff80_0000) | 1,
            (upper & !0x1ff) | ((top_mem2 >> 32) as u32 & 0xff),
            0x0100_0000,
        )
    } else {
        (lower, upper, 0x0100_0000)
    }
}

fn dram_above_4g(smn: &Smn<impl ConfigRegionAccess>, top_mem2: u64) {
    let lower = smn.reg(NB_LOWER_TOP_OF_DRAM2);
    let upper = smn.reg(NB_UPPER_TOP_OF_DRAM2);
    let (low, high, mmio_base) = top_of_dram2(lower.get(), upper.get(), top_mem2);
    lower.set(low);
    upper.set(high);
    smn.reg(NB_TOP_OF_DRAM3).set(0x83ff_ffff);
    smn.reg(NB_MMIOBASE).set(mmio_base);
    smn.reg(NB_MMIOLIMIT).set(0x7fff_ffff);
    smn.reg(NB_PCI_CTRL).modify(NbPciCtrl::MMIO_ENABLE::SET);
}

/// Tell the IOHC of a die about the DRAM map and the MMIO above 4 GiB.
/// `top_mem` is TOP_MEM (MSR C001_001A), `top_mem2` TOP_MEM2 if SYSCFG
/// enables it.
pub fn dram_map<S: Soc, P: ConfigRegionAccess + Copy>(
    die: &Die<S, P>,
    top_mem: u32,
    top_mem2: Option<u64>,
) {
    let root = PciAddress::new(0, die.bus(), 0, 0);
    // SAFETY: D0F0 of the die's root bus; exclusivity is the `Die` contract.
    let tom = unsafe {
        ConfigRegister::<_, TopOfDram::Register>::new(die.pci(), root, NB_TOP_OF_DRAM_SLOT1)
    };
    tom.modify(TopOfDram::TOM.val(top_mem >> 23));
    dram_above_4g(&die.smn(), top_mem2.unwrap_or(0));
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

    #[test]
    fn tom2_below_and_above_the_hypertransport_hole() {
        // 32 GiB: TOM2 in the IOHC, MMIO from 16 MiB up.
        assert_eq!(
            top_of_dram2(0x0000_0002, 0x8000_0000, 0x8_0000_0000),
            (0x0000_0003, 0x8000_0008, 0x0100_0000)
        );
        // TOM2 above 1 TiB: the IOHC stops at the hole, MMIO base above TOM2.
        assert_eq!(
            top_of_dram2(0, 0, 0x101_0000_0000),
            (0xff80_0001, 0x1ff, 0x0101_0000)
        );
        assert_eq!(top_of_dram2(7, 9, 0), (7, 9, 0x0100_0000));
        let space = SmnSpace::default();
        dram_above_4g(&space.smn(), 0);
        assert_eq!(space.get(NB_PCI_CTRL), 1 << 23);
        assert_eq!(space.get(NB_MMIOBASE), 0x0100_0000);
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
