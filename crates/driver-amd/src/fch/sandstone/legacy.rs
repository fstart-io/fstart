// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native FCH legacy/RTC/internal IRQ/LPC policy; no PCI enumeration or SMM.
use crate::fch::acpimmio::{
    PM_0A, PM_S5_RESET_STATUS, Pm0a, Pm1StatusHigh, PmIo, S5ResetStatus, legacy_sd_off, pm,
    smi_bank,
};
use crate::fch::irq;
use crate::fch::smbus::SMBUS_IO;
use crate::fch::{
    FchHw,
    lpc::{self, ClockControl, LpcA4, PciControl},
    rtc,
};
use crate::window::{At, Window};
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

register_bitfields![u32,
    /// PMx40: eSPI interrupt masks.
    EspiInterruptMask [
        BIT1 OFFSET(1) NUMBITS(1) [],
        BIT12 OFFSET(12) NUMBITS(1) []
    ]
];
register_bitfields![u8,
    /// PCI_INTR index 8: the source of the IOAPIC inputs shared with legacy
    /// devices. IRQ14/15 come from the legacy IDE unless routed: then the
    /// internal sources [`initialize`] maps to them reach the IOAPIC.
    IntrMiscMap [
        IRQ1_ROUTED OFFSET(1) NUMBITS(1) [],
        IRQ12_ROUTED OFFSET(3) NUMBITS(1) [],
        IRQ14 OFFSET(4) NUMBITS(2) [
            LegacyIde = 0,
            Routed = 3
        ],
        IRQ15 OFFSET(6) NUMBITS(2) [
            LegacyIde = 0,
            Routed = 3
        ]
    ],
];
const PM_ESPI_INTR_MASK: crate::window::At<u32, EspiInterruptMask::Register> = pm::at(0x40);
/// The PM1 status high byte of Picasso's ACPI block at 0x400.
const PM1_STATUS_HIGH: u16 = 0x401;

/// The IOAPIC input (ISA IRQ) [`initialize`] routes the internal source
/// with PCI_INTR index `index` to.
pub fn internal_irq(index: u8) -> Option<u8> {
    INTERNAL_IRQS
        .iter()
        .find(|&&(source, _)| source == index)
        .map(|&(_, irq)| irq)
}

const INTERNAL_IRQS: [(u8, u8); 19] = [
    (0, 0x10),
    (1, 0x11),
    (2, 0x12),
    (3, 0x13),
    (irq::SCI, crate::fch::SCI_IRQ),
    (0x17, 0x10),
    (0x1a, 0x10),
    (irq::EMMC, 5),
    (irq::GPIO, 7),
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
pub fn disable_rom_sharing(fch: &impl FchHw) {
    fch.lpc()
        .at(lpc::PCI_CONTROL)
        .modify(PciControl::VW_ROM_SHARING_EN::CLEAR + PciControl::EXT_ROM_SHARING_EN::CLEAR);
}
pub fn lpc_init(fch: &impl FchHw) {
    let lpc = fch.lpc();
    lpc.at(lpc::RA4).modify(LpcA4::BIT0::SET);
    lpc.at(lpc::CLOCK_CONTROL).modify(ClockControl::BIT31::SET);
}
/// ACPI ports/SMBus must already be decoded. These are temporary native routing
/// defaults; platform/board IRQ tables replace them after enumeration.
/// Bank0 RTC accesses follow native C and enable NMI; install exception/NMI
/// handling before this step. No calendar/checksum update is performed.
pub fn initialize<H: FchHw>(fch: &H, espi: bool, legacy_sd: bool) {
    let (acpi, io) = (fch.acpi(), fch.io());
    if espi {
        acpi.at(PM_ESPI_INTR_MASK)
            .modify(EspiInterruptMask::BIT1::CLEAR + EspiInterruptMask::BIT12::CLEAR);
    }
    let pm = PmIo(io);
    let pm0a = pm.at(PM_0A);
    pm0a.modify(Pm0a::BIT3::SET);
    pm0a.modify(Pm0a::BIT4::SET);
    // Clears the 4-second power button status (write 1 to clear).
    pm.at(PM_S5_RESET_STATUS)
        .modify(S5ResetStatus::FOUR_SECOND_PWR_BTN::SET);
    io.at(At::<u8>::new((SMBUS_IO + 0x14).into())).set(0); // SMBus autopoll off
    rtc::register::<_, rtc::Control::Register>(io, rtc::CONTROL)
        .modify(rtc::Control::DST_EN::CLEAR);
    rtc::register::<_, rtc::FreqSelect::Register>(io, rtc::FREQ_SELECT)
        .modify(rtc::FreqSelect::BIT4::CLEAR);
    let route = |index| irq::entry(io, index);
    crate::window::indexed::<_, u8, IntrMiscMap::Register>(io, irq::INDEX, irq::DATA, irq::MISC)
        .modify(
            IntrMiscMap::IRQ1_ROUTED::SET
                + IntrMiscMap::IRQ12_ROUTED::SET
                + IntrMiscMap::IRQ14::Routed
                + IntrMiscMap::IRQ15::Routed,
        );
    let misc0 = route(irq::MISC0).get();
    route(irq::MISC0).set((misc0 & 0x99) | 0x10);
    io.at(At::<u8>::new((SMBUS_IO + 0x2a).into())).set(0x2f); // ASF W1C status
    let smi_enable = acpi.at(smi_bank::at::<u32, ()>(4)).get();
    let wake = io.at(At::<u8, Pm1StatusHigh::Register>::new(
        PM1_STATUS_HIGH.into(),
    ));
    if smi_enable & acpi.at(smi_bank::at::<u32, ()>(0)).get() != 0 {
        wake.write(Pm1StatusHigh::PCIEXP_WAKE::SET);
    }
    let status = wake.extract();
    if status.is_set(Pm1StatusHigh::PWRBTN) || status.is_set(Pm1StatusHigh::RTC) {
        wake.write(Pm1StatusHigh::PCIEXP_WAKE::SET);
    }
    for (index, apic) in INTERNAL_IRQS {
        route(irq::index(index, irq::Mode::Apic)).set(apic);
    }
    if !legacy_sd {
        legacy_sd_off(acpi);
    }
}
