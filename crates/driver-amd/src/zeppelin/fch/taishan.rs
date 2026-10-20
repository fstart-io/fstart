// SPDX-License-Identifier: MIT OR Apache-2.0
//! Die 0's FCH: the vendor FchTaishanPei reset tasks, the FchTaishanDxe env
//! and late tasks and the pre-console setup. The SATA and USB controllers
//! are in [`super::sata`] and [`super::usb`].
use super::{
    AB_INDEX, AL2AHB, ASF_IO, Acpimmio, Bus, Reg, SMBUS_IO, Space, abcfg_update, index_read,
    index_write, irq, legacy_sd_off,
};
use crate::{
    fam17h::registers::{Smn, SmnReg},
    fch::{
        Decode,
        early::{self, Registers},
    },
    zeppelin::{Revision, acpi_io},
};
use tock_registers::{interfaces::Writeable, register_bitfields};

/// Pre-console FCH setup, after
/// [`crate::fch::enable_acpimmio`]: LPC decodes off and the SPI BAR, port 80
/// if the board posts there, SMBus, CF9 and legacy I/O. The SPI read mode and
/// speeds stay as the PSP left them; the vendor firmware keeps them too.
pub fn pre_init(fch: &mut impl Registers, port80: bool) -> Result<(), early::Error> {
    early::prepare_lpc(fch)?;
    if port80 {
        early::enable_port80(fch);
    }
    early::initialize_smbus(fch)?;
    early::enable_cf9_io(fch);
    early::enable_legacy_io(fch);
    Ok(())
}

const DIE0: Acpimmio = Acpimmio::DIE0;

#[derive(Clone, Copy)]
enum Bank {
    Pm,
    Misc,
    Smi,
}
/// A byte read-clear-set of an ACPI MMIO register.
#[derive(Clone, Copy)]
struct Rmw {
    bank: Bank,
    register: u8,
    clear: u8,
    set: u8,
}
const fn pm(register: u8, clear: u8, set: u8) -> Rmw {
    Rmw {
        bank: Bank::Pm,
        register,
        clear,
        set,
    }
}
const fn misc(register: u8, clear: u8, set: u8) -> Rmw {
    Rmw {
        bank: Bank::Misc,
        ..pm(register, clear, set)
    }
}
const fn smi(register: u8, clear: u8, set: u8) -> Rmw {
    Rmw {
        bank: Bank::Smi,
        ..pm(register, clear, set)
    }
}
fn apply(bus: &mut impl Bus, table: &[Rmw]) {
    for rmw in table {
        let offset = u32::from(rmw.register);
        let reg = match rmw.bank {
            Bank::Pm => DIE0.pm::<u8, ()>(offset),
            Bank::Misc => DIE0.misc(offset),
            Bank::Smi => DIE0.smi(offset),
        };
        bus.update(reg, rmw.clear, rmw.set);
    }
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
    /// PMx54.
    SerialIrq [MODE OFFSET(6) NUMBITS(1) [], ENABLE OFFSET(7) NUMBITS(1) []]
];
const RTC_FREQ_SELECT: u8 = 0x0a;
const RTC_CONTROL: u8 = 0x0b;
const RTC_VALID: u8 = 0x0d;
const PM_ACPI_SMI_CMD: u32 = 0x6a;

fn hwacpi_reset(bus: &mut impl Bus) {
    // ACPI PM blocks, the SMI command port, PM2 control at 0xba.
    for (offset, port) in [
        (0x60, acpi_io::PM1_EVT),
        (0x62, acpi_io::PM1_CNT),
        (0x64, acpi_io::PM_TMR),
        (0x66, acpi_io::CSTATE_CONTROL),
        (0x68, acpi_io::GPE0),
        (PM_ACPI_SMI_CMD, 0xb2),
        (0x6e, 0x00ba),
        (0x6c, 0xffff),
    ] {
        bus.set(DIE0.pm::<u16, ()>(offset), port);
    }
    bus.update(DIE0.pm::<u32, ()>(0), 0x0018_0000, 0);
    bus.set(Reg::<u8>::io(SMBUS_IO + 0x14), 0); // SMBus autopoll off
    // SMBus offset 0x12 through the ACPI MMIO alias.
    bus.set(DIE0.smbus::<u8, ()>(0x12), 1);
    apply(bus, PEI_ACPIMMIO_MAIN);
    bus.update(DIE0.pm::<u8, ()>(0), 0, 1 << 7); // vendor PCD 0xa1

    // RTC: no daylight saving, register A bit 4 clear.
    let control = index_read(bus, super::RTC_INDEX, RTC_CONTROL);
    index_write(
        bus,
        super::RTC_INDEX,
        RTC_CONTROL,
        control & !RtcB::DST_EN::SET.value,
    );
    let select = index_read(bus, super::RTC_INDEX, RTC_FREQ_SELECT);
    index_write(bus, super::RTC_INDEX, RTC_FREQ_SELECT, select & !(1 << 4));
    if index_read(bus, super::RTC_INDEX, RTC_VALID) & 0x3f == 0x3f {
        let valid = index_read(bus, super::RTC_INDEX, RTC_VALID);
        index_write(bus, super::RTC_INDEX, RTC_VALID, valid & 0x80);
    }

    let misc = irq::read(bus, irq::MISC, irq::Mode::Pic);
    irq::write(bus, irq::MISC, irq::Mode::Pic, misc | 0x0a);
    let misc0 = irq::read(bus, irq::MISC0, irq::Mode::Pic);
    irq::write(bus, irq::MISC0, irq::Mode::Pic, (misc0 & 0x99) | 0x10);

    bus.set(Reg::<u8>::io(ASF_IO + 0x0e), 0x70);
    bus.set(Reg::<u8>::io(ASF_IO + 0x0a), 0x2f);

    let pm1_status_high = Reg::<u8>::io(acpi_io::PM1_EVT + 1);
    let smi_pending = bus.get(DIE0.smi::<u32, ()>(4)).get() & bus.get(DIE0.smi::<u32, ()>(0)).get();
    if smi_pending != 0 || bus.get(pm1_status_high).get() & 5 != 0 {
        bus.set(pm1_status_high, 0x40);
    }

    // Serial IRQ: mode bit cleared (vendor PCD 0x95), then enabled.
    bus.modify(
        DIE0.pm::<u8, SerialIrq::Register>(0x54),
        SerialIrq::MODE::CLEAR + SerialIrq::ENABLE::SET,
    );
}

register_bitfields![u32,
    SpiCntrl0 [READ_MODE_LOW OFFSET(18) NUMBITS(1) [], READ_MODE_HIGH OFFSET(29) NUMBITS(2) []]
];
register_bitfields![u16,
    Spi100Speed [NORMAL OFFSET(12) NUMBITS(4) [Mhz33 = 1]],
    Spi100HostPrefetch [RD4DW_EN_HOST OFFSET(15) NUMBITS(1) []]
];

/// The vendor SPI reset task with its defaults: SPI100 on, normal speed
/// 33 MHz, fast speed left alone (PCD 0x9a = 0), read mode 7 (fast read, PCD
/// 0x98) and no 4-dword host prefetch. The vendor EFS lookup only matters
/// when the policy read mode is 0, and its Micron dummy-cycle quirk (flash ID
/// 0x20 stored in CMOS-RAM by the DXE SPI task) isn't ported.
fn spi_reset(bus: &mut impl Bus) {
    const FAST_READ: u32 = 7;
    let spi = super::super::SPI_BASE;
    let bar = bus.get(Reg::<u32>::lpc(0xa0)).get();
    bus.set(Reg::<u32>::lpc(0xa0), (bar & 0x1f) | spi);
    bus.update(Reg::<u32>::mmio(spi), 0, 0x0708_0000);
    bus.update(Reg::<u32>::mmio(spi + 0x0c), 0x003f_0000, 0);
    bus.update(Reg::<u8>::mmio(spi + 0x20), 0, 1); // SPI100 enable
    bus.modify(
        Reg::<u16, Spi100Speed::Register>::mmio(spi + 0x22),
        Spi100Speed::NORMAL::Mhz33,
    );
    bus.update(Reg::<u32>::mmio(spi + 0x1c), 1 << 10, 0);
    bus.update(
        Reg::<u16, Spi100HostPrefetch::Register>::mmio(spi + 0x2c),
        Spi100HostPrefetch::RD4DW_EN_HOST::SET.value,
        1 << 14,
    );
    bus.modify(
        Reg::<u32, SpiCntrl0::Register>::mmio(spi),
        SpiCntrl0::READ_MODE_HIGH.val(FAST_READ >> 1) + SpiCntrl0::READ_MODE_LOW.val(FAST_READ & 1),
    );
    // LPC ROM DMA control: two separate word writes, as the vendor code.
    bus.update(Reg::<u16>::lpc(0xba), 0, 1 << 8);
    bus.update(Reg::<u16>::lpc(0xba), 0, 1 << 7);
}

/// The FchTaishanPei reset tasks of die 0's FCH. `revision` selects the A0
/// workaround of PMx08.
pub fn reset_init(bus: &mut impl Bus, revision: Revision) {
    // Prelim
    bus.update(DIE0.pm::<u8, ()>(0x0a), 0, 1 << 3);
    bus.update(DIE0.aoac::<u32, ()>(0x94), 0xc000, 0);
    bus.update(DIE0.pm::<u8, ()>(0x0a), 0, 1 << 4);
    bus.update(DIE0.pm::<u8, ()>(0xc8), 1 << 4, 0);
    if revision == Revision::A0 {
        bus.update(DIE0.pm::<u8, ()>(0x08), 0x60, 0x60);
    }
    bus.update(DIE0.pm::<u8, ()>(0xd2), 1 << 6, 0);
    bus.update(DIE0.pm::<u8, ()>(0xc0), 0, 1 << 1); // clears the reset status

    // A-Link index/data at 0xcd8, kept enabled.
    bus.set(DIE0.pm::<u32, ()>(0xe0), u32::from(AB_INDEX));

    spi_reset(bus);
    hwacpi_reset(bus);

    // LPC
    bus.set(Reg::<u8>::lpc(0x6c), 0);
    bus.update(Reg::<u8>::lpc(0x7c), 0, 0x05);
    bus.update(Reg::<u8>::lpc(0xba), 0x60, 0x60);
    bus.update(Reg::<u8>::lpc(0xa4), 0, 0x01);
    bus.update(Reg::<u8>::lpc(0xd1), 0, 0x60);
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

/// MISCx40: clock control 1.
const MISC_CLK_CNTL1: u32 = 0x40;

/// Clock generator: the same values as Picasso, through the ACPI MMIO window.
fn clock_init(bus: &mut impl Bus) {
    for (offset, clear, set) in [
        (0x10, 0x0000_1ff3, 0),
        (0x34, 0xff80_0000, 0),
        (0x14, 0x00ff_ff00, 0),
        (0x18, 0xffff_0000, 0),
        (
            0x1c,
            0x0c1f_fe00,
            (4 << 13) | (3 << 17) | (3 << 26) | (1 << 9),
        ),
        (MISC_CLK_CNTL1, 0, (1 << 25) | (1 << 30)),
        (0x10, 0, 1 << 29),
        (0x08, 0, 1), // CG1PLL spread spectrum
    ] {
        bus.update(DIE0.misc::<u32, ()>(offset), clear, set);
    }
}

/// A-Link: the six table rows, then the policy settings of the vendor code.
const ABCFG: &[(u32, u32, u32)] = &[
    (0x00080, 0x0006_0000, 0x0006_0001),
    (0x00090, 0x0020_0000, 0x0020_0000),
    (0x00094, 0x0010_0000, 0x0010_0fee),
    (0x10054, 0x00ff_ffff, 0x0000_07ff),
    (0x10060, 0xfbff_ffff, 0x0200_0000),
    (0x10090, 0x0001_0000, 0x0001_0000),
    (0x10050, 0x0000_0004, 0x0000_0000),
    (0x00054, 0x00ff_0000, 0x0010_0000),
    (0x10054, 0x00ff_0000, 0x0010_0000),
    (0x00054, 0x0000_0010, 0x0000_0010),
    (0x00054, 0x0100_0000, 0x0100_0000),
    (0x10054, 0x0300_0000, 0x0300_0000),
    (0x00054, 0x0000_0005, 0x0000_0004),
    (0x000bc, 0x0000_0010, 0x0000_0010),
    (0x00058, 0xa000_0000, 0xa000_0000),
    (0x00208, 0x0000_0280, 0x0000_0280),
    (0x00208, 0x0040_8010, 0x0040_8010),
    (0x00180, 0x0000_1f21, 0x0000_0000),
    (0x00184, 0x0000_0004, 0x0000_0000),
    (0x0022c, 0x0000_4800, 0x0000_0000),
];

fn ab_init(bus: &mut impl Bus) {
    bus.update(DIE0.misc::<u32, ()>(MISC_CLK_CNTL1), 1 << 1, 0);
    bus.set(DIE0.pm::<u32, ()>(0xe0), u32::from(AB_INDEX));
    for &(register, clear, set) in ABCFG {
        abcfg_update(bus, register, clear, set);
    }
    bus.update(DIE0.misc::<u8, ()>(0x68), (1 << 1) | (1 << 2), 0);
    bus.update(DIE0.misc::<u8, ()>(0x2e), 0, (1 << 0) | (1 << 1));
    bus.update(DIE0.pm::<u8, ()>(0x06), 0, 1);
}

/// The xHCI subsystem ID strap of die 0 (NBIF).
const NBIF_XHCI_SSID: SmnReg = SmnReg::new(0x1014_304c);

/// The FchTaishanDxe env tasks of die 0's FCH. `smn` is die 0's;
/// `subsystem_id` (subsystem vendor in the low, device in the high half) is the
/// board's for the xHCI (vendor PCD 0x163).
pub fn env_init(
    bus: &mut impl Bus,
    smn: &Smn<impl fstart_pci::ConfigRegionAccess>,
    subsystem_id: u32,
) {
    clock_init(bus);
    apply(bus, ENV_MISC);

    // HwAcpi
    bus.set(Reg::<u8>::new(Space::Smbus, 0x10), 0);
    bus.set(Reg::<u8>::new(Space::Smbus, 0x11), 0);
    apply(bus, ENV_HWACPI);
    bus.modify(
        DIE0.pm::<u32, Decode::Register>(0),
        Decode::HPET::SET + Decode::HPET_MSI::CLEAR,
    );
    bus.update(Reg::<u8>::mmio(AL2AHB + 0x10), 0, 1 << 1);
    bus.update(Reg::<u8>::mmio(AL2AHB + 0x30), 0, 1 << 1);
    // Legacy UART decode: none enabled.
    bus.set(Reg::<u16>::mmio(AL2AHB + 0x20), 0xe400);

    // LPC
    bus.update(Reg::<u8>::lpc(0x40), 0x04, 0x04);
    bus.update(Reg::<u8>::lpc(0x78), 0x03, 0);
    bus.update(Reg::<u8>::lpc(0xbb), 0x0c, 0x0c);

    legacy_sd_off(bus, DIE0);

    smn.reg(NBIF_XHCI_SSID).set(subsystem_id);

    ab_init(bus);
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
pub fn late_init(bus: &mut impl Bus) {
    bus.update(DIE0.pm::<u32, ()>(0xa0), !0xc07f_00a0, 0);
    for &(index, apic) in INTERNAL_IRQ_ROUTING {
        irq::write(bus, index, irq::Mode::Apic, apic);
    }
}
