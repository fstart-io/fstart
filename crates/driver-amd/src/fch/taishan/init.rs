// SPDX-License-Identifier: MIT OR Apache-2.0
//! Die 0's FCH: the vendor FchTaishanPei reset tasks, the FchTaishanDxe env
//! and late tasks and the pre-console setup. The SATA and USB controllers
//! are in [`super::sata`] and [`super::usb`].
use super::acpi_io;
use crate::cpu::Revision;
use crate::fch::irq;
use crate::{
    fch::{
        AL2AHB, FchHw, SPI_BASE,
        acpimmio::{
            self, AOAC_94, AbRegBar, Aoac94, CgPllConfig1, CgPllConfig3, Decode,
            MISC_CGPLL_CONFIG1, MISC_CGPLL_CONFIG3, MISC_CLK_CNTL1, MiscClkCntl1, PM_0A,
            PM_AB_REG_BAR, PM_ACPI_CPU_CNT, PM_ACPI_GPE0, PM_ACPI_PM_TMR, PM_ACPI_PM1_CNT,
            PM_ACPI_PM1_EVT, PM_ACPI_PM2_CNT, PM_ACPI_PMA_CNT, PM_ACPI_SMI_CMD, PM_DECODE,
            PM_S5_RESET_STATUS, PM_SERIAL_IRQ, Pm0a, Pm1StatusHigh, S5ResetStatus, SerialIrq, misc,
            pm, smbus, smi_bank,
        },
        alink, early,
        lpc::{self, Bits, PciControl, RomDmaControl, SpiBase},
        rtc,
        smbus::{ASF_IO, SMBUS_IO},
        spi, uart,
    },
    smn::{Smn, SmnReg},
    table::{Record, rmw},
    window::{At, Window},
};
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

/// Pre-console FCH setup, after
/// [`crate::fch::acpimmio::enable_acpimmio`]: LPC decodes off and the SPI BAR, port 80
/// if the board posts there, SMBus, CF9 and legacy I/O. The SPI read mode and
/// speeds stay as the PSP left them; the vendor firmware keeps them too.
pub fn pre_init(fch: &impl FchHw, port80: bool) -> Result<(), early::Error> {
    early::prepare_lpc(fch)?;
    if port80 {
        // SAFETY: the caller owns the FCH, LPC decodes included.
        lpc::enable_port80(fch);
    }
    early::initialize_smbus(fch)?;
    early::enable_cf9_io(fch);
    early::enable_legacy_io(fch);
    Ok(())
}

/// A byte read-clear-set of an ACPI MMIO register: the vendor tables,
/// whose bits mostly have no known meaning.
type Rmw = Record<At<u8>, u8>;
const fn pm(register: u8, clear: u8, set: u8) -> Rmw {
    rmw(pm::at(register as u32), clear, set)
}
const fn misc(register: u8, clear: u8, set: u8) -> Rmw {
    rmw(misc::at(register as u32), clear, set)
}
const fn smi(register: u8, clear: u8, set: u8) -> Rmw {
    rmw(smi_bank::at(register as u32), clear, set)
}

// ---- PEI reset tasks ----

/// pei_acpimmio_main of the vendor HwAcpi reset task.
const PEI_ACPIMMIO_MAIN: &[Rmw] = &[
    pm(0x5d, 0x03, 0x02),
    pm(0xd2, 0x30, 0x00),
    pm(0x47, 0x98, 0x88),
    pm(0x22, 0xff, 0xff),
    pm(0x23, 0x07, 0x07),
    pm(0x00, 0x08, 0x77),
    pm(0xec, 0x04, 0x04),
    pm(0x08, 0x01, 0x14),
    pm(0x09, 0x00, 0x01),
    pm(0x10, 0x03, 0x00),
    pm(0x54, 0xff, 0x50),
    pm(0x74, 0x09, 0x01),
    pm(0xc4, 0x11, 0x04),
    pm(0xc1, 0x07, 0x04),
    pm(0xc2, 0x40, 0x40),
    pm(0xbe, 0x20, 0x20),
    pm(0xc4, 0x04, 0x00),
    misc(0x6e, 0x80, 0x80),
    misc(0x6f, 0x08, 0x08),
];

register_bitfields![u8,
    /// PMx08: the A0 workaround.
    Pm08 [A0 OFFSET(5) NUMBITS(2) []],
    Pm0c8 [BIT4 OFFSET(4) NUMBITS(1) []],
    Pm0d2 [BIT6 OFFSET(6) NUMBITS(1) []],
    /// PMx00 bit 7 (vendor PCD 0xa1).
    Pm00 [PCD_A1 OFFSET(7) NUMBITS(1) []],
    Pm06 [BIT0 OFFSET(0) NUMBITS(1) []],
    Misc68 [
        BIT1 OFFSET(1) NUMBITS(1) [],
        BIT2 OFFSET(2) NUMBITS(1) []
    ],
    Misc2e [
        BIT0 OFFSET(0) NUMBITS(1) [],
        BIT1 OFFSET(1) NUMBITS(1) []
    ],
    /// SMBus host and ASF controller registers.
    SmbusAutopoll [ENABLE OFFSET(0) NUMBITS(8) []],
];
register_bitfields![u32,
    /// PMx00 bits 19 and 20.
    Pm00High [BITS OFFSET(19) NUMBITS(2) []]
];
/// PM2 control at 0xba; the PMA block isn't used.
const PM2_CNT: u16 = 0x00ba;
const PMA_CNT: u16 = 0xffff;

fn hwacpi_reset(fch: &impl FchHw) {
    // ACPI PM blocks, the SMI command port, PM2 control at 0xba.
    for (reg, port) in [
        (PM_ACPI_PM1_EVT, acpi_io::PM1_EVT),
        (PM_ACPI_PM1_CNT, acpi_io::PM1_CNT),
        (PM_ACPI_PM_TMR, acpi_io::PM_TMR),
        (PM_ACPI_CPU_CNT, acpi_io::CSTATE_CONTROL),
        (PM_ACPI_GPE0, acpi_io::GPE0),
        (PM_ACPI_SMI_CMD, crate::fch::smi::APM_CNT),
        (PM_ACPI_PM2_CNT, PM2_CNT),
        (PM_ACPI_PMA_CNT, PMA_CNT),
    ] {
        fch.acpi().at(reg).set(port);
    }
    fch.acpi()
        .at(pm::at::<u32, Pm00High::Register>(0))
        .modify(Pm00High::BITS.val(0));
    fch.io()
        .at(At::<u8, SmbusAutopoll::Register>::new(
            (SMBUS_IO + 0x14).into(),
        ))
        .set(0);
    // SMBus offset 0x12 through the ACPI MMIO alias.
    fch.acpi().at(smbus::at::<u8, ()>(0x12)).set(1);
    acpimmio::apply(fch.acpi(), PEI_ACPIMMIO_MAIN.iter().copied());
    fch.acpi()
        .at(pm::at::<u8, Pm00::Register>(0))
        .modify(Pm00::PCD_A1::SET);

    // RTC: no daylight saving, register A bit 4 clear.
    rtc::register::<_, rtc::Control::Register>(fch.io(), rtc::CONTROL)
        .modify(rtc::Control::DST_EN::CLEAR);
    rtc::register::<_, rtc::FreqSelect::Register>(fch.io(), rtc::FREQ_SELECT)
        .modify(rtc::FreqSelect::BIT4::CLEAR);
    let valid = rtc::register::<_, rtc::Valid::Register>(fch.io(), rtc::VALID);
    if valid.read(rtc::Valid::VALID) == 0x3f {
        valid.modify(rtc::Valid::VALID.val(0) + rtc::Valid::BIT6::CLEAR);
    }

    let misc = irq::read(fch.io(), irq::index(irq::MISC, irq::Mode::Pic));
    irq::write(fch.io(), irq::index(irq::MISC, irq::Mode::Pic), misc | 0x0a);
    let misc0 = irq::read(fch.io(), irq::index(irq::MISC0, irq::Mode::Pic));
    irq::write(
        fch.io(),
        irq::index(irq::MISC0, irq::Mode::Pic),
        (misc0 & 0x99) | 0x10,
    );

    fch.io().at(At::<u8>::new((ASF_IO + 0x0e).into())).set(0x70);
    fch.io().at(At::<u8>::new((ASF_IO + 0x0a).into())).set(0x2f);

    let pm1_status_high = fch.io().at(At::<u8, Pm1StatusHigh::Register>::new(
        (acpi_io::PM1_EVT + 1).into(),
    ));
    let smi_pending = fch.acpi().at(smi_bank::at::<u32, ()>(4)).get()
        & fch.acpi().at(smi_bank::at::<u32, ()>(0)).get();
    let wake = || {
        let status = pm1_status_high.extract();
        status.is_set(Pm1StatusHigh::PWRBTN) || status.is_set(Pm1StatusHigh::RTC)
    };
    if smi_pending != 0 || wake() {
        pm1_status_high.write(Pm1StatusHigh::PCIEXP_WAKE::SET);
    }

    // Serial IRQ: mode bit cleared (vendor PCD 0x95), then enabled.
    fch.acpi()
        .at(PM_SERIAL_IRQ)
        .modify(SerialIrq::QUIET_MODE::CLEAR + SerialIrq::ENABLE::SET);
}

/// The vendor SPI reset task with its defaults: SPI100 on, normal speed
/// 33 MHz, fast speed left alone (PCD 0x9a = 0), read mode 7 (fast read, PCD
/// 0x98) and no 4-dword host prefetch. The vendor EFS lookup only matters
/// when the policy read mode is 0, and its Micron dummy-cycle quirk (flash ID
/// 0x20 stored in CMOS-RAM by the DXE SPI task) isn't ported.
fn spi_reset(fch: &impl FchHw) {
    const FAST_READ: u32 = 7;
    // The SPI BAR, its reserved bits cleared.
    fch.lpc()
        .at(lpc::SPI_BASE_ADDRESS)
        .modify(SpiBase::BASE.val(SPI_BASE >> 8) + SpiBase::RESERVED.val(0));
    let cntrl0 = fch.mmio().at(spi::CNTRL0);
    cntrl0.modify(spi::Cntrl0::BIT19::SET + spi::Cntrl0::ARB_WAIT_COUNT.val(7));
    fch.mmio()
        .at(spi::CNTRL1)
        .modify(spi::Cntrl1::WAIT_COUNT.val(0));
    fch.mmio()
        .at(spi::SPI100_ENABLE_BYTE)
        .modify(spi::Spi100EnableByte::ENABLE::SET);
    fch.mmio()
        .at(spi::SPI100_SPEED)
        .modify(spi::Spi100Speed::NORMAL::Mhz33);
    fch.mmio()
        .at(spi::ALT_SPI_CS)
        .modify(spi::AltSpiCs::WRITE_BUFFER_EN::CLEAR);
    fch.mmio().at(spi::SPI100_HOST_PREFETCH).modify(
        spi::Spi100HostPrefetch::RD4DW_EN_HOST::CLEAR + spi::Spi100HostPrefetch::HOST_BURST_EN::SET,
    );
    cntrl0.modify(
        spi::Cntrl0::READ_MODE_HIGH.val(FAST_READ >> 1)
            + spi::Cntrl0::READ_MODE_LOW.val(FAST_READ & 1),
    );
    // LPC ROM DMA control: two separate word writes, as the vendor code.
    fch.lpc()
        .at(lpc::ROM_DMA_CONTROL)
        .modify(RomDmaControl::BIT8::SET);
    fch.lpc()
        .at(lpc::ROM_DMA_CONTROL)
        .modify(RomDmaControl::BIT7::SET);
}

/// The FchTaishanPei reset tasks of die 0's FCH. `revision` selects the A0
/// workaround of PMx08.
pub fn reset_init(fch: &impl FchHw, revision: Revision) {
    // Prelim
    fch.acpi().at(PM_0A).modify(Pm0a::BIT3::SET);
    fch.acpi().at(AOAC_94).modify(Aoac94::SX.val(0));
    fch.acpi().at(PM_0A).modify(Pm0a::BIT4::SET);
    fch.acpi()
        .at(pm::at::<u8, Pm0c8::Register>(0xc8))
        .modify(Pm0c8::BIT4::CLEAR);
    if revision == Revision::ZpA0 {
        fch.acpi()
            .at(pm::at::<u8, Pm08::Register>(0x08))
            .modify(Pm08::A0.val(3));
    }
    fch.acpi()
        .at(pm::at::<u8, Pm0d2::Register>(0xd2))
        .modify(Pm0d2::BIT6::CLEAR);
    fch.acpi()
        .at(PM_S5_RESET_STATUS)
        .modify(S5ResetStatus::FOUR_SECOND_PWR_BTN::SET);

    // A-Link index/data at 0xcd8, kept enabled.
    fch.acpi()
        .at(PM_AB_REG_BAR)
        .write(AbRegBar::PORT.val(alink::INDEX.into()));

    spi_reset(fch);
    hwacpi_reset(fch);

    // LPC
    fch.lpc().at(lpc::R6C).set(0);
    fch.lpc().at(lpc::R7C).modify(Bits::B0::SET + Bits::B2::SET);
    fch.lpc()
        .at(lpc::ROM_DMA_CONTROL_BYTE)
        .modify(Bits::B5::SET + Bits::B6::SET);
    fch.lpc().at(lpc::RA4_BYTE).modify(Bits::B0::SET);
    fch.lpc()
        .at(lpc::CLOCK_CONTROL_BYTE1)
        .modify(Bits::B5::SET + Bits::B6::SET);
}

// ---- DXE env tasks ----

const ENV_MISC: &[Rmw] = &[
    pm(0xcc, 0x07, 0x03),
    pm(0x74, 0xff, 0x17),
    pm(0x77, 0x20, 0x00),
    pm(0xba, 0x08, 0x08),
    pm(0xbc, 0x02, 0x02),
    // SCI map: SCI source n -> GEVENT
    smi(0x41, 0xff, 0x01),
    smi(0x43, 0xff, 0x03),
    smi(0x44, 0xff, 0x04),
    smi(0x45, 0xff, 0x05),
    smi(0x46, 0xff, 0x06),
    smi(0x57, 0xff, 0x17),
    smi(0x48, 0xff, 0x18),
    smi(0x65, 0xff, 0x1e),
    smi(0x66, 0xff, 0x1f),
    smi(0x70, 0xff, 0x09),
    smi(0x79, 0xff, 0x0b),
    smi(0x6c, 0xff, 0x0d),
    smi(0x5d, 0xff, 0x10),
    smi(0x5e, 0xff, 0x11),
    smi(0x08, 0x18, 0x00),
    smi(0x0e, 0x08, 0x08),
];

const ENV_HWACPI: &[Rmw] = &[
    pm(0xed, 0x02, 0x02),
    pm(0xbb, 0x40, 0x00),
    pm(0x77, 0x0b, 0x08),
    pm(0x02, 0x04, 0x04),
    pm(0x81, 0x18, 0x18),
    pm(0x94, 0xff, 0x01),
    pm(0x83, 0x21, 0x20),
    misc(0x6e, 0xa0, 0xa0),
];

/// Clock generator: the same values as Picasso, through the ACPI MMIO window,
/// in the order of the vendor FchTaishanDxe (VA 0x7a98). The CG1 PLL spreads
/// the PCIe reference clocks if `spread_spectrum` (vendor PCD 0x120, Setup
/// "SB Clock Spread Spectrum"); the second update request applies it.
fn clock_init(fch: &impl FchHw, spread_spectrum: bool) {
    acpimmio::apply::<u32>(
        fch.acpi(),
        [
            rmw(misc::at(0x10), 0x0000_1ff3, 0),
            rmw(misc::at(0x34), 0xff80_0000, 0),
            rmw(misc::at(0x14), 0x00ff_ff00, 0),
            rmw(misc::at(0x18), 0xffff_0000, 0),
            rmw(
                misc::at(0x1c),
                0x0c1f_fe00,
                (4 << 13) | (3 << 17) | (3 << 26) | (1 << 9),
            ),
        ],
    );
    let cntl1 = fch.acpi().at(MISC_CLK_CNTL1);
    cntl1.modify(MiscClkCntl1::CG1_FBDIV_LOADEN::SET);
    cntl1.modify(MiscClkCntl1::CG1_CFG_UPDATE_REQ::SET);
    fch.acpi()
        .at(MISC_CGPLL_CONFIG3)
        .modify(CgPllConfig3::CG1_FRACN_EN_OVERRIDE::SET);
    fch.acpi()
        .at(MISC_CGPLL_CONFIG1)
        .modify(CgPllConfig1::CG1_SPREAD_SPECTRUM.val(spread_spectrum.into()));
    cntl1.modify(MiscClkCntl1::CG1_CFG_UPDATE_REQ::SET);
}

/// A-Link: the six table rows, then the policy settings of the vendor code.
const ABCFG: &[alink::Rmw] = &[
    rmw(0x00080, 0x0006_0000, 0x0006_0001),
    rmw(0x00090, 0x0020_0000, 0x0020_0000),
    rmw(0x00094, 0x0010_0000, 0x0010_0fee),
    rmw(0x10054, 0x00ff_ffff, 0x0000_07ff),
    rmw(0x10060, 0xfbff_ffff, 0x0200_0000),
    rmw(0x10090, 0x0001_0000, 0x0001_0000),
    rmw(0x10050, 0x0000_0004, 0x0000_0000),
    rmw(0x00054, 0x00ff_0000, 0x0010_0000),
    rmw(0x10054, 0x00ff_0000, 0x0010_0000),
    rmw(0x00054, 0x0000_0010, 0x0000_0010),
    rmw(0x00054, 0x0100_0000, 0x0100_0000),
    rmw(0x10054, 0x0300_0000, 0x0300_0000),
    rmw(0x00054, 0x0000_0005, 0x0000_0004),
    rmw(0x000bc, 0x0000_0010, 0x0000_0010),
    rmw(0x00058, 0xa000_0000, 0xa000_0000),
    rmw(0x00208, 0x0000_0280, 0x0000_0280),
    rmw(0x00208, 0x0040_8010, 0x0040_8010),
    rmw(0x00180, 0x0000_1f21, 0x0000_0000),
    rmw(0x00184, 0x0000_0004, 0x0000_0000),
    rmw(0x0022c, 0x0000_4800, 0x0000_0000),
];

fn ab_init(fch: &impl FchHw) {
    fch.acpi()
        .at(MISC_CLK_CNTL1)
        .modify(MiscClkCntl1::ALINK_GATE::CLEAR);
    fch.acpi()
        .at(PM_AB_REG_BAR)
        .write(AbRegBar::PORT.val(alink::INDEX.into()));
    alink::apply(fch.io(), ABCFG);
    fch.acpi()
        .at(misc::at::<u8, Misc68::Register>(0x68))
        .modify(Misc68::BIT1::CLEAR + Misc68::BIT2::CLEAR);
    fch.acpi()
        .at(misc::at::<u8, Misc2e::Register>(0x2e))
        .modify(Misc2e::BIT0::SET + Misc2e::BIT1::SET);
    fch.acpi()
        .at(pm::at::<u8, Pm06::Register>(0x06))
        .modify(Pm06::BIT0::SET);
}

/// The xHCI subsystem ID strap of die 0 (NBIF).
const NBIF_XHCI_SSID: SmnReg = SmnReg::new(0x1014_304c);

register_bitfields![u8,
    /// AL2AHB +0x10 and +0x30.
    Al2ahbControl [BIT1 OFFSET(1) NUMBITS(1) []]
];

/// The FchTaishanDxe env tasks of die 0's FCH. `smn` is die 0's;
/// `subsystem_id` (subsystem vendor in the low, device in the high half) is the
/// board's for the xHCI (vendor PCD 0x163); `spread_spectrum` the board's
/// CG1 PLL spread-spectrum choice.
pub fn env_init(
    fch: &impl FchHw,
    smn: &Smn<impl fstart_pci::ConfigRegionAccess>,
    subsystem_id: u32,
    spread_spectrum: bool,
) {
    clock_init(fch, spread_spectrum);
    acpimmio::apply(fch.acpi(), ENV_MISC.iter().copied());

    // HwAcpi
    fch.smbus().at(At::<u8>::new(0x10)).set(0);
    fch.smbus().at(At::<u8>::new(0x11)).set(0);
    acpimmio::apply(fch.acpi(), ENV_HWACPI.iter().copied());
    fch.acpi()
        .at(PM_DECODE)
        .modify(Decode::HPET::SET + Decode::HPET_MSI::CLEAR);
    for offset in [0x10, 0x30] {
        fch.mmio()
            .at(At::<u8, Al2ahbControl::Register>::new(AL2AHB + offset))
            .modify(Al2ahbControl::BIT1::SET);
    }
    // Legacy UART decode: none enabled.
    uart::set_legacy_decode(fch, 0xe400);

    // LPC
    fch.lpc()
        .at(lpc::PCI_CONTROL)
        .modify(PciControl::LEGACY_DMA_EN::SET);
    fch.lpc()
        .at(lpc::MISC_CONTROL)
        .modify(Bits::B0::CLEAR + Bits::B1::CLEAR);
    fch.lpc()
        .at(lpc::ROM_DMA_CONTROL_HIGH)
        .modify(Bits::B2::SET + Bits::B3::SET);

    acpimmio::legacy_sd_off(fch.acpi());

    smn.reg(NBIF_XHCI_SSID).set(subsystem_id);

    ab_init(fch);
}

// ---- DXE late tasks ----

/// IOAPIC routing of the internal devices (vendor table 9250).
const INTERNAL_IRQ_ROUTING: &[(u8, u8)] = &[
    (irq::A, 0x10),
    (irq::B, 0x11),
    (irq::C, 0x12),
    (irq::D, 0x13),
    (irq::SD, 0x10),
    (irq::SDIO, 0x10),
    (irq::EMMC, 0x05),
    (irq::GPIO, 0x07),
    (irq::UART0, 0x03),
    (irq::UART1, 0x04),
    (irq::UART2, 0x03),
    (irq::UART3, 0x04),
    (irq::I2C0, 0x0a),
    (irq::I2C1, 0x0b),
    (irq::I2C2, 0x04),
    (irq::I2C3, 0x06),
    (irq::I2C4, 0x0e),
    (irq::I2C5, 0x0f),
];

/// The FchTaishanDxe late (ReadyToBoot) tasks of die 0's FCH.
pub fn late_init(fch: &impl FchHw) {
    acpimmio::apply::<u32>(fch.acpi(), [rmw(pm::at(0xa0), !0xc07f_00a0, 0)]);
    for &(index, apic) in INTERNAL_IRQ_ROUTING {
        irq::write(fch.io(), irq::index(index, irq::Mode::Apic), apic);
    }
}
