//! PEI reset tasks shared by Taishan and Huashan (FCH_SP3 §3.1).

use fstart_core::indexed::Indexed;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

use super::acpi_mmio_table;
use crate::cpu::Silicon;
use crate::fch::acpimmio::{
    self, AOAC_94, ASF, Aoac94, AoacDevice, IOMUX, IsaControl, MISC, PM_0A, PM_AB_REG_BAR,
    PM_ACPI_CPU_CNT, PM_ACPI_GPE0, PM_ACPI_PM_TMR, PM_ACPI_PM1_CNT, PM_ACPI_PM1_EVT,
    PM_ACPI_PM2_CNT, PM_ACPI_PMA_CNT, PM_ACPI_SMI_CMD, PM_D2, PM_DECODE, PM_ISA_CONTROL, PM_MISC,
    PM_RTC_EXT_DATA, PM_RTC_EXT_INDEX, PM_S5_RESET_STATUS, PM_SERIAL_IRQ, PM1_STS, Pm1Control,
    PmIo, SerialIrq,
};
use crate::fch::irq;
use crate::fch::lpc::{IO_MEM_DECODE, WIDEIO_GENERIC_PORT};
use crate::fch::smbus::{ASF_IO, SMBUS_IO};
use crate::fch::{FchHw, rtc};
use crate::window::{At, Window, apply, update};

/// Milan IOMUX pins set to function 0 in the prelim task (FCH_SP3 §3.1
/// af4f), in donor order.
const MILAN_IOMUX_FN0: [u8; 12] = [
    0x15, 0x20, 0x68, 0x69, 0x6A, 0x6B, 0x4A, 0x58, 0x4B, 0x57, 0x6D, 0x1F,
];
/// Milan eMMC block byte set in prelim (`fedd58bb` = 0x88; unnamed).
const MILAN_EMMC_BB: u32 = 0xFEDD_58BB;

/// af4f: ACPI-MMIO decode on, clear the Sx AOAC bits and the S5 status.
/// Runs through PMIO because ACPI-MMIO decode may still be off.
pub fn prelim<H: FchHw>(hw: &H, generation: Silicon) {
    let pmio = PmIo(hw.io());
    if generation == Silicon::Zp {
        update::<u8, _>(&pmio, PM_0A.offset, 0, 0x08);
    }
    pmio.at(PM_ISA_CONTROL).modify(IsaControl::MMIO_EN::SET);
    hw.acpi().at(AOAC_94).modify(Aoac94::SX::CLEAR);
    update::<u8, _>(&pmio, PM_0A.offset, 0, 0x10);
    update::<u8, _>(&pmio, PM_MISC.offset, 0x10, 0);
    update::<u8, _>(&pmio, PM_D2.offset, 0x40, 0);
    if generation == Silicon::Gn {
        MILAN_IOMUX_FN0
            .iter()
            .for_each(|&pin| hw.acpi().write8(IOMUX + u32::from(pin), 0));
        hw.mmio().write8(MILAN_EMMC_BB, 0x88);
    }
    // PMxC0 byte: write-1-to-clear FourSecondPwrBtn.
    pmio.write8(PM_S5_RESET_STATUS.offset, 0x02);
    if generation == Silicon::Gn {
        // MISCx50 bit 20 pulse (unnamed).
        update::<u32, _>(hw.acpi(), MISC + 0x50, 0, 1 << 20);
        update::<u32, _>(hw.acpi(), MISC + 0x50, 1 << 20, 0);
    }
}

/// af41: AB register BAR = `CD8h`.
pub fn ab_bar<H: FchHw>(hw: &H) {
    hw.acpi()
        .reg::<u32, ()>(PM_AB_REG_BAR.offset)
        .set(super::super::alink::INDEX.into());
}

/// The ACPI I/O map ResetHwAcpi programs (PMx60..6E); FADT uses the same
/// values (ACPI_SMM_SP3 §3.1).
pub const PM_IO_MAP: [(At<u16>, u16); 8] = [
    (PM_ACPI_PM1_EVT, acpimmio::PM1_STS as u16),
    (PM_ACPI_PM1_CNT, acpimmio::PM1_CNT as u16),
    (PM_ACPI_PM_TMR, acpimmio::PM_TMR as u16),
    (PM_ACPI_CPU_CNT, acpimmio::CPU_CNT as u16),
    (PM_ACPI_GPE0, acpimmio::GPE0_STS as u16),
    (PM_ACPI_SMI_CMD, crate::fch::smi::APM_CNT),
    (PM_ACPI_PM2_CNT, acpimmio::PM2_CNT as u16),
    // PMx6C: the PMA block, unused.
    (PM_ACPI_PMA_CNT, 0xFFFF),
];

/// af40: SMBus/ASF and ACPI I/O decode, the ACPI-MMIO table, RTC, PCI_INTR
/// 8/9, ASF, wake status and serial IRQ.
pub fn hw_acpi<H: FchHw>(hw: &H, generation: Silicon) {
    let (acpi, io, lpc) = (hw.acpi(), hw.io(), hw.lpc());
    // Wide I/O 0 at E00h (enabled on Huashan here, on ZP by the LPC task).
    lpc.reg::<u16, ()>(WIDEIO_GENERIC_PORT.offset).set(0x0E00);
    if generation != Silicon::Zp {
        lpc.write8(IO_MEM_DECODE.offset, 0x04);
    }
    // PmDecodeEn: SMBus/ASF I/O at B00h, enabled.
    let decode = SMBUS_IO >> 8 << 8 | 0x10;
    update::<u16, _>(acpi, PM_DECODE.offset, 0xF400, decode);
    PM_IO_MAP
        .iter()
        .for_each(|&(reg, port)| acpi.at(reg).set(port));
    update::<u32, _>(acpi, PM_DECODE.offset, 0x0018_F400, decode.into());
    io.write8(u32::from(SMBUS_IO) + 0x14, 0);
    apply(acpi, acpi_mmio_table(generation));
    rtc_extension(acpi, io);
    // RTC register 0Ah: clear DV0 (bit 4).
    rtc::register::<_, rtc::FreqSelect::Register>(io, rtc::FREQ_SELECT)
        .modify(rtc::FreqSelect::BIT4::CLEAR);
    irq::update(io, irq::MISC, 0, 0x0A);
    irq::update(io, irq::MISC0, 0x66, 0x10);
    if generation == Silicon::Zp {
        io.write8(u32::from(ASF_IO) + 0x0E, 0x70);
    }
    io.write8(u32::from(ASF_IO) + 0x0A, 0x2F);
    // Clear a pending PM1_STS[14] wake status.
    if io.read8(PM1_STS + 1) & 0x40 != 0 {
        io.write8(PM1_STS + 1, 0x40);
    }
    let serirq = acpi.at(PM_SERIAL_IRQ);
    serirq.write(SerialIrq::NUM_SER_IRQ_BITS.val(4));
    serirq.write(SerialIrq::NUM_SER_IRQ_BITS.val(4) + SerialIrq::ENABLE::SET);
    if generation != Silicon::Zp {
        // ASF+15h bit 4 (Huashan only; unnamed).
        update::<u8, _>(acpi, ASF + 0x15, 0, 0x10);
    }
}

/// The RTC extension sequence of af40 (PMx5E/5F index/data), decoded from
/// the 00/ff emulation traces: index 1 is sampled, index 0 is set to 1; if
/// index 1 read FFh (battery loss) extension indices 1–4 and 10h–14h are
/// zeroed. Index 0 is set again around the RTC register B write and
/// cleared after it. The extension indices are not publicly named.
fn rtc_extension<A: Window, I: Window>(acpi: &A, io: &I) {
    let ext = |index: u8| {
        Indexed::<_, _, ()>::new(
            acpi.reg::<u8, ()>(PM_RTC_EXT_INDEX.offset),
            acpi.reg::<u8, ()>(PM_RTC_EXT_DATA.offset),
            index,
        )
    };
    let lost = ext(1).get() == 0xFF;
    ext(0).set(1);
    if lost {
        [1, 2, 3, 4, 0x10, 0x11, 0x12, 0x13, 0x14]
            .into_iter()
            .for_each(|i| ext(i).set(0));
        ext(0).set(1);
    }
    // RTC register 0Bh: clear DSE (bit 0) with the extension enabled.
    rtc::register::<_, rtc::Control::Register>(io, rtc::CONTROL)
        .modify(rtc::Control::DST_EN::CLEAR);
    ext(0).set(0);
}

/// af44: eSPI. At the donor defaults eSPI is off on all generations: AOAC
/// device 1Bh is powered off (FCH_SP3 §3.1).
pub fn espi<H: FchHw>(hw: &H, enabled: bool) {
    if !enabled {
        acpimmio::aoac_power(hw.acpi(), AoacDevice::ESPI, false);
    }
}

/// Whether the platform resumes from S3 (PM1_CNT SLP_TYP = 3 at reset;
/// FCH_SP3 §2.2 boot mode: 3 → S3, 4 → S4, 5 → S5).
pub fn resume_sleep_type<H: FchHw>(hw: &H) -> u8 {
    hw.io()
        .reg::<u16, Pm1Control::Register>(acpimmio::PM1_CNT)
        .read(Pm1Control::SLP_TYP) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fch::acpimmio::PM;
    use crate::fch::model::fch;

    #[test]
    fn prelim_rome_through_pmio() {
        let hw = fch();
        prelim(&hw, Silicon::Ssp);
        let pm = |r: u32| hw.io.at(0xcd7, r);
        assert_eq!(pm(0x04) & 2, 2);
        assert_eq!(pm(0x0a), 0x10);
        assert_eq!(pm(0xc0), 0x02);
        assert!(hw.mmio.writes().is_empty());
    }

    #[test]
    fn prelim_milan_iomux_and_pulse() {
        let hw = fch();
        prelim(&hw, Silicon::Gn);
        assert_eq!(hw.acpi.writes_to(0xd6d), vec![0]);
        assert_eq!(hw.mmio.writes(), vec![(MILAN_EMMC_BB, 1, 0x88)]);
        assert_eq!(hw.acpi.writes_to(0xe50), vec![1 << 20, 0]);
    }

    #[test]
    fn hw_acpi_rome_io_map_and_battery_loss() {
        let hw = fch();
        // RTC extension index 1 reads FFh: battery loss path.
        hw.acpi
            .0
            .borrow_mut()
            .reads
            .insert(PM + 0x5f, vec![0xff, 0]);
        hw_acpi(&hw, Silicon::Ssp);
        assert_eq!(hw.acpi.get(PM + 0x6a, 2), 0xb2);
        assert_eq!(hw.acpi.get(PM + 0x60, 2), 0x800);
        assert_eq!(hw.acpi.get(PM, 4) & 0xff00, 0x0b00);
        assert_eq!(hw.acpi.get(PM + 0x54, 1), 0x90);
        assert_eq!(hw.acpi.writes_to(PM + 0x5e).len(), 13);
        assert_eq!(hw.io.at(0xc01, 9), 0x10);
        assert_eq!(hw.acpi.get(0x915, 1), 0x10);
    }
}
