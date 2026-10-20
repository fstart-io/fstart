// SPDX-License-Identifier: MIT OR Apache-2.0
//! Die 0's FCH: the vendor FchTaishanPei reset tasks, the FchTaishanDxe env
//! and late tasks and the pre-console setup. The SATA and USB controllers
//! are in [`super::sata`] and [`super::usb`].
use super::{ASF_IO, SMBUS_IO, irq};
use crate::{
    fch::{
        AL2AHB, AbRegBar, Bus, Decode, Fch, LEGACY_UART_DECODE, MISC_CLK_CNTL1,
        MiscClkCntl1, PM_AB_REG_BAR, PM_ACPI_CPU_CNT, PM_ACPI_GPE0,
        PM_ACPI_PM_TMR, PM_ACPI_PM1_CNT, PM_ACPI_PM1_EVT, PM_ACPI_PM2_CNT, PM_ACPI_PMA_CNT,
        AcpiReg, PM_0A, PM_ACPI_SMI_CMD, PM_C0, PM_DECODE, Pm0a, PmC0, RTC_INDEX, Reg, SPI_BASE, alink,
        early, indexed_io, misc, spi,
        aoac, pm, smbus, smi_bank as smi,
        lpc::{self, Bits, PciControl, RomDmaControl, SpiBase},
    },
    smn::{Smn, SmnReg},
    table::{Record, rmw},
    zeppelin::{Revision, acpi_io},
};
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

/// Pre-console FCH setup, after
/// [`crate::fch::enable_acpimmio`]: LPC decodes off and the SPI BAR, port 80
/// if the board posts there, SMBus, CF9 and legacy I/O. The SPI read mode and
/// speeds stay as the PSP left them; the vendor firmware keeps them too.
pub fn pre_init(fch: &Fch<impl Bus>, port80: bool) -> Result<(), early::Error> {
    early::prepare_lpc(fch)?;
    if port80 {
        // SAFETY: the caller owns the FCH, LPC decodes included.
        unsafe { lpc::Lpc::new(fch.bus()) }.enable_port80();
    }
    early::initialize_smbus(fch)?;
    early::enable_cf9_io(fch);
    early::enable_legacy_io(fch);
    Ok(())
}

/// A byte read-clear-set of an ACPI MMIO register: the vendor tables,
/// whose bits mostly have no known meaning.
type Rmw = Record<AcpiReg<u8>, u8>;
const fn pm(register: u8, clear: u8, set: u8) -> Rmw {
    rmw(pm::at(register as u32), clear, set)
}
const fn misc(register: u8, clear: u8, set: u8) -> Rmw {
    rmw(misc::at(register as u32), clear, set)
}
const fn smi(register: u8, clear: u8, set: u8) -> Rmw {
    rmw(smi::at(register as u32), clear, set)
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
    RtcB [DST_EN OFFSET(0) NUMBITS(1) []],
    RtcA [BIT4 OFFSET(4) NUMBITS(1) []],
    RtcD [VALID OFFSET(0) NUMBITS(6) [], BIT6 OFFSET(6) NUMBITS(1) [], VRT OFFSET(7) NUMBITS(1) []],
    /// PMx54.
    SerialIrq [MODE OFFSET(6) NUMBITS(1) [], ENABLE OFFSET(7) NUMBITS(1) []],
    /// PMx08: the A0 workaround.
    Pm08 [A0 OFFSET(5) NUMBITS(2) []],
    Pm0c8 [BIT4 OFFSET(4) NUMBITS(1) []],
    Pm0d2 [BIT6 OFFSET(6) NUMBITS(1) []],
    /// PMx00 bit 7 (vendor PCD 0xa1).
    Pm00 [PCD_A1 OFFSET(7) NUMBITS(1) []],
    Pm06 [BIT0 OFFSET(0) NUMBITS(1) []],
    Misc68 [BIT1 OFFSET(1) NUMBITS(1) [], BIT2 OFFSET(2) NUMBITS(1) []],
    Misc2e [BIT0 OFFSET(0) NUMBITS(1) [], BIT1 OFFSET(1) NUMBITS(1) []],
    /// SMBus host and ASF controller registers.
    SmbusAutopoll [ENABLE OFFSET(0) NUMBITS(8) []],
    /// The PM1 status bits 8 (power button) and 10 (RTC) of the high byte.
    Pm1StatusHigh [PWRBTN OFFSET(0) NUMBITS(1) [], RTC OFFSET(2) NUMBITS(1) [],
        WAKE OFFSET(6) NUMBITS(1) []]
];
register_bitfields![u32,
    /// AOACx94.
    pub(super) Aoac94 [BITS OFFSET(14) NUMBITS(2) []],
    /// PMx00 bits 19 and 20.
    Pm00High [BITS OFFSET(19) NUMBITS(2) []]
];
const RTC_FREQ_SELECT: u8 = 0x0a;
const RTC_CONTROL: u8 = 0x0b;
const RTC_VALID: u8 = 0x0d;
pub(super) const AOAC_94: crate::fch::AcpiReg<u32, Aoac94::Register> = aoac::at(0x94);
/// PM2 control at 0xba; the PMA block isn't used.
const PM2_CNT: u16 = 0x00ba;
const PMA_CNT: u16 = 0xffff;
const SMI_CMD: u16 = 0xb2;

fn hwacpi_reset(fch: &Fch<impl Bus>) {
    let bus = fch.bus();
    // ACPI PM blocks, the SMI command port, PM2 control at 0xba.
    for (reg, port) in [
        (PM_ACPI_PM1_EVT, acpi_io::PM1_EVT),
        (PM_ACPI_PM1_CNT, acpi_io::PM1_CNT),
        (PM_ACPI_PM_TMR, acpi_io::PM_TMR),
        (PM_ACPI_CPU_CNT, acpi_io::CSTATE_CONTROL),
        (PM_ACPI_GPE0, acpi_io::GPE0),
        (PM_ACPI_SMI_CMD, SMI_CMD),
        (PM_ACPI_PM2_CNT, PM2_CNT),
        (PM_ACPI_PMA_CNT, PMA_CNT),
    ] {
        fch.reg(reg).set(port);
    }
    fch.reg(pm::at::<u32, Pm00High::Register>(0))
        .modify(Pm00High::BITS.val(0));
    bus.reg(Reg::<u8, SmbusAutopoll::Register>::io(SMBUS_IO + 0x14))
        .set(0);
    // SMBus offset 0x12 through the ACPI MMIO alias.
    fch.reg(smbus::at::<u8, ()>(0x12)).set(1);
    fch.apply(PEI_ACPIMMIO_MAIN.iter().copied());
    fch.reg(pm::at::<u8, Pm00::Register>(0))
        .modify(Pm00::PCD_A1::SET);

    // RTC: no daylight saving, register A bit 4 clear.
    indexed_io::<_, RtcB::Register>(bus, RTC_INDEX, RTC_CONTROL).modify(RtcB::DST_EN::CLEAR);
    indexed_io::<_, RtcA::Register>(bus, RTC_INDEX, RTC_FREQ_SELECT).modify(RtcA::BIT4::CLEAR);
    let valid = indexed_io::<_, RtcD::Register>(bus, RTC_INDEX, RTC_VALID);
    if valid.read(RtcD::VALID) == 0x3f {
        valid.modify(RtcD::VALID.val(0) + RtcD::BIT6::CLEAR);
    }

    let misc = irq::read(bus, irq::MISC, irq::Mode::Pic);
    irq::write(bus, irq::MISC, irq::Mode::Pic, misc | 0x0a);
    let misc0 = irq::read(bus, irq::MISC0, irq::Mode::Pic);
    irq::write(bus, irq::MISC0, irq::Mode::Pic, (misc0 & 0x99) | 0x10);

    bus.reg(Reg::<u8>::io(ASF_IO + 0x0e)).set(0x70);
    bus.reg(Reg::<u8>::io(ASF_IO + 0x0a)).set(0x2f);

    let pm1_status_high = bus.reg(Reg::<u8, Pm1StatusHigh::Register>::io(acpi_io::PM1_EVT + 1));
    let smi_pending =
        fch.reg(smi::at::<u32, ()>(4)).get() & fch.reg(smi::at::<u32, ()>(0)).get();
    let wake = || {
        let status = pm1_status_high.extract();
        status.is_set(Pm1StatusHigh::PWRBTN) || status.is_set(Pm1StatusHigh::RTC)
    };
    if smi_pending != 0 || wake() {
        pm1_status_high.write(Pm1StatusHigh::WAKE::SET);
    }

    // Serial IRQ: mode bit cleared (vendor PCD 0x95), then enabled.
    fch.reg(pm::at::<u8, SerialIrq::Register>(0x54))
        .modify(SerialIrq::MODE::CLEAR + SerialIrq::ENABLE::SET);
}

/// The vendor SPI reset task with its defaults: SPI100 on, normal speed
/// 33 MHz, fast speed left alone (PCD 0x9a = 0), read mode 7 (fast read, PCD
/// 0x98) and no 4-dword host prefetch. The vendor EFS lookup only matters
/// when the policy read mode is 0, and its Micron dummy-cycle quirk (flash ID
/// 0x20 stored in CMOS-RAM by the DXE SPI task) isn't ported.
fn spi_reset(bus: &impl Bus) {
    const FAST_READ: u32 = 7;
    // The SPI BAR, its reserved bits cleared.
    bus.reg(lpc::SPI_BASE_ADDRESS)
        .modify(SpiBase::BASE.val(SPI_BASE >> 8) + SpiBase::RESERVED.val(0));
    let cntrl0 = bus.reg(spi::CNTRL0);
    cntrl0.modify(spi::Cntrl0::BIT19::SET + spi::Cntrl0::BITS24.val(7));
    bus.reg(spi::RESTRICTED_CMD2)
        .modify(spi::RestrictedCmd2::BITS.val(0));
    bus.reg(spi::SPI100_ENABLE_BYTE)
        .modify(spi::Spi100EnableByte::ENABLE::SET);
    bus.reg(spi::SPI100_SPEED)
        .modify(spi::Spi100Speed::NORMAL::Mhz33);
    bus.reg(spi::ALT_SPI_CS)
        .modify(spi::AltSpiCs::BIT10::CLEAR);
    bus.reg(spi::SPI100_HOST_PREFETCH).modify(
        spi::Spi100HostPrefetch::RD4DW_EN_HOST::CLEAR + spi::Spi100HostPrefetch::BIT14::SET,
    );
    cntrl0.modify(
        spi::Cntrl0::READ_MODE_HIGH.val(FAST_READ >> 1)
            + spi::Cntrl0::READ_MODE_LOW.val(FAST_READ & 1),
    );
    // LPC ROM DMA control: two separate word writes, as the vendor code.
    bus.reg(lpc::ROM_DMA_CONTROL)
        .modify(RomDmaControl::BIT8::SET);
    bus.reg(lpc::ROM_DMA_CONTROL)
        .modify(RomDmaControl::BIT7::SET);
}

/// The FchTaishanPei reset tasks of die 0's FCH. `revision` selects the A0
/// workaround of PMx08.
pub fn reset_init(fch: &Fch<impl Bus>, revision: Revision) {
    let bus = fch.bus();
    // Prelim
    fch.reg(pm::at::<u8, Pm0a::Register>(PM_0A.into()))
        .modify(Pm0a::BIT3::SET);
    fch.reg(AOAC_94).modify(Aoac94::BITS.val(0));
    fch.reg(pm::at::<u8, Pm0a::Register>(PM_0A.into()))
        .modify(Pm0a::BIT4::SET);
    fch.reg(pm::at::<u8, Pm0c8::Register>(0xc8))
        .modify(Pm0c8::BIT4::CLEAR);
    if revision == Revision::A0 {
        fch.reg(pm::at::<u8, Pm08::Register>(0x08))
            .modify(Pm08::A0.val(3));
    }
    fch.reg(pm::at::<u8, Pm0d2::Register>(0xd2))
        .modify(Pm0d2::BIT6::CLEAR);
    fch.reg(pm::at::<u8, PmC0::Register>(PM_C0.into()))
        .modify(PmC0::CLEAR_RESET_STATUS::SET);

    // A-Link index/data at 0xcd8, kept enabled.
    fch.reg(PM_AB_REG_BAR)
        .write(AbRegBar::PORT.val(alink::INDEX.into()));

    spi_reset(bus);
    hwacpi_reset(fch);

    // LPC
    bus.reg(lpc::R6C).set(0);
    bus.reg(lpc::R7C).modify(Bits::B0::SET + Bits::B2::SET);
    bus.reg(lpc::ROM_DMA_CONTROL_BYTE)
        .modify(Bits::B5::SET + Bits::B6::SET);
    bus.reg(lpc::RA4_BYTE).modify(Bits::B0::SET);
    bus.reg(lpc::CLOCK_CONTROL_BYTE1)
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

/// Clock generator: the same values as Picasso, through the ACPI MMIO window.
fn clock_init(fch: &Fch<impl Bus>) {
    fch.apply::<u32>([
        rmw(misc::at(0x10), 0x0000_1ff3, 0),
        rmw(misc::at(0x34), 0xff80_0000, 0),
        rmw(misc::at(0x14), 0x00ff_ff00, 0),
        rmw(misc::at(0x18), 0xffff_0000, 0),
        rmw(
            misc::at(0x1c),
            0x0c1f_fe00,
            (4 << 13) | (3 << 17) | (3 << 26) | (1 << 9),
        ),
        rmw(misc::at(0x40), 0, (1 << 25) | (1 << 30)),
        rmw(misc::at(0x10), 0, 1 << 29),
        rmw(misc::at(0x08), 0, 1), // CG1PLL spread spectrum
    ]);
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

fn ab_init(fch: &Fch<impl Bus>) {
    fch.reg(MISC_CLK_CNTL1)
        .modify(MiscClkCntl1::ALINK_GATE::CLEAR);
    fch.reg(PM_AB_REG_BAR)
        .write(AbRegBar::PORT.val(alink::INDEX.into()));
    alink::apply(fch.bus(), ABCFG);
    fch.reg(misc::at::<u8, Misc68::Register>(0x68))
        .modify(Misc68::BIT1::CLEAR + Misc68::BIT2::CLEAR);
    fch.reg(misc::at::<u8, Misc2e::Register>(0x2e))
        .modify(Misc2e::BIT0::SET + Misc2e::BIT1::SET);
    fch.reg(pm::at::<u8, Pm06::Register>(0x06))
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
/// board's for the xHCI (vendor PCD 0x163).
pub fn env_init(
    fch: &Fch<impl Bus>,
    smn: &Smn<impl fstart_pci::ConfigRegionAccess>,
    subsystem_id: u32,
) {
    let bus = fch.bus();
    clock_init(fch);
    fch.apply(ENV_MISC.iter().copied());

    // HwAcpi
    bus.reg(Reg::<u8>::smbus(0x10)).set(0);
    bus.reg(Reg::<u8>::smbus(0x11)).set(0);
    fch.apply(ENV_HWACPI.iter().copied());
    fch.reg(PM_DECODE)
        .modify(Decode::HPET::SET + Decode::HPET_MSI::CLEAR);
    for offset in [0x10, 0x30] {
        bus.reg(Reg::<u8, Al2ahbControl::Register>::mmio(AL2AHB + offset))
            .modify(Al2ahbControl::BIT1::SET);
    }
    // Legacy UART decode: none enabled.
    bus.reg(LEGACY_UART_DECODE).set(0xe400);

    // LPC
    bus.reg(lpc::PCI_CONTROL)
        .modify(PciControl::LEGACY_DMA_EN::SET);
    bus.reg(lpc::MISC_CONTROL)
        .modify(Bits::B0::CLEAR + Bits::B1::CLEAR);
    bus.reg(lpc::ROM_DMA_CONTROL_HIGH)
        .modify(Bits::B2::SET + Bits::B3::SET);

    fch.legacy_sd_off();

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
pub fn late_init(fch: &Fch<impl Bus>) {
    fch.apply::<u32>([rmw(pm::at(0xa0), !0xc07f_00a0, 0)]);
    for &(index, apic) in INTERNAL_IRQ_ROUTING {
        irq::write(fch.bus(), index, irq::Mode::Apic, apic);
    }
}
