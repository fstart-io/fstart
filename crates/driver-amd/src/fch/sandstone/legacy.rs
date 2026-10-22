// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native FCH legacy/RTC/internal IRQ/LPC policy; no PCI enumeration or SMM.
use crate::fch::{
    Bus, Fch, PM_0A, PM_C0, PM_INDEX, Pm0a, PmC0, RTC_INDEX, Reg, SMBUS_ASF_IO_BASE, indexed_io,
    lpc::{self, ClockControl, LpcA4, PciControl},
    pm, smi_bank,
};
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

register_bitfields![u32,
    /// PMx40: eSPI interrupt masks.
    EspiInterruptMask [BIT1 OFFSET(1) NUMBITS(1) [], BIT12 OFFSET(12) NUMBITS(1) []]
];
register_bitfields![u8,
    RtcB [DST_EN OFFSET(0) NUMBITS(1) []],
    RtcA [BIT4 OFFSET(4) NUMBITS(1) []],
    /// The PM1 status bits 8 (power button) and 10 (RTC) of the high byte.
    Pm1StatusHigh [PWRBTN OFFSET(0) NUMBITS(1) [], RTC OFFSET(2) NUMBITS(1) [],
        WAKE OFFSET(6) NUMBITS(1) []]
];
const PM_ESPI_INTR_MASK: crate::fch::AcpiReg<u32, EspiInterruptMask::Register> = pm::at(0x40);
/// The PCI_INTR index/data pair.
const PCI_INTR_INDEX: u16 = 0xc00;
/// The PM1 status high byte of Picasso's ACPI block at 0x400.
const PM1_STATUS_HIGH: u16 = 0x401;

const INTERNAL_IRQS: [(u8, u8); 18] = [
    (0, 0x10),
    (1, 0x11),
    (2, 0x12),
    (3, 0x13),
    (0x17, 0x10),
    (0x1a, 0x10),
    (0x43, 5),
    (0x62, 7),
    (0x76, 3),
    (0x77, 4),
    (0x78, 15),
    (0x79, 5),
    (0x70, 10),
    (0x71, 11),
    (0x72, 14),
    (0x73, 6),
    (0x74, 14),
    (0x75, 15),
];
/// Disable ROM sharing BEFORE the board changes GPIO67 to the DMIC mux.
/// BYTE PCI access is intentional: adjacent control bytes are not touched.
pub fn disable_rom_sharing(bus: &impl Bus) {
    bus.reg(lpc::PCI_CONTROL).modify(
        PciControl::VW_ROM_SHARING_EN::CLEAR + PciControl::EXT_ROM_SHARING_EN::CLEAR,
    );
}
pub fn lpc_init(bus: &impl Bus) {
    bus.reg(lpc::RA4).modify(LpcA4::BIT0::SET);
    bus.reg(lpc::CLOCK_CONTROL)
        .modify(ClockControl::BIT31::SET);
}
/// ACPI ports/SMBus must already be decoded. These are temporary native routing
/// defaults; platform/board IRQ tables replace them after enumeration.
/// Bank0 RTC accesses follow native C and enable NMI; install exception/NMI
/// handling before this step. No calendar/checksum update is performed.
pub fn initialize<B: Bus>(fch: &Fch<B>, espi: bool, legacy_sd: bool) {
    let bus = fch.bus();
    if espi {
        fch.reg(PM_ESPI_INTR_MASK)
            .modify(EspiInterruptMask::BIT1::CLEAR + EspiInterruptMask::BIT12::CLEAR);
    }
    let pm0a = indexed_io::<B, Pm0a::Register>(bus, PM_INDEX, PM_0A);
    pm0a.modify(Pm0a::BIT3::SET);
    pm0a.modify(Pm0a::BIT4::SET);
    indexed_io::<B, PmC0::Register>(bus, PM_INDEX, PM_C0)
        .modify(PmC0::CLEAR_RESET_STATUS::SET);
    bus.reg(Reg::<u8>::io(SMBUS_ASF_IO_BASE + 0x14)).set(0); // SMBus autopoll off
    indexed_io::<B, RtcB::Register>(bus, RTC_INDEX, 0x0b).modify(RtcB::DST_EN::CLEAR);
    indexed_io::<B, RtcA::Register>(bus, RTC_INDEX, 0x0a).modify(RtcA::BIT4::CLEAR);
    let irq = |index| indexed_io::<B, ()>(bus, PCI_INTR_INDEX, index);
    irq(8).set(irq(8).get() | 0x0a);
    let misc0 = irq(9).get();
    irq(9).set((misc0 & 0x99) | 0x10);
    bus.reg(Reg::<u8>::io(SMBUS_ASF_IO_BASE + 0x2a)).set(0x2f); // ASF W1C status
    let smi_enable = fch.reg(smi_bank::at::<u32, ()>(4)).get();
    let wake = bus.reg(Reg::<u8, Pm1StatusHigh::Register>::io(PM1_STATUS_HIGH));
    if smi_enable & fch.reg(smi_bank::at::<u32, ()>(0)).get() != 0 {
        wake.write(Pm1StatusHigh::WAKE::SET);
    }
    let status = wake.extract();
    if status.is_set(Pm1StatusHigh::PWRBTN) || status.is_set(Pm1StatusHigh::RTC) {
        wake.write(Pm1StatusHigh::WAKE::SET);
    }
    for (index, apic) in INTERNAL_IRQS {
        irq(index | 0x80).set(apic);
    }
    if !legacy_sd {
        fch.legacy_sd_off();
    }
}
