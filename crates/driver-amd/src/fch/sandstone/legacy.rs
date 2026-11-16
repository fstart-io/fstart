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

/// The routes of the sources every board has: INTA-D, the SCI, SD, SDIO,
/// eMMC and the GPIO controller.
const FIXED_IRQS: [(u8, u8); 9] = [
    (irq::A, 0x10),
    (irq::B, 0x11),
    (irq::C, 0x12),
    (irq::D, 0x13),
    (irq::SCI, crate::fch::SCI_IRQ),
    (irq::SD, 0x10),
    (irq::SDIO, 0x10),
    (irq::EMMC, 5),
    (irq::GPIO, 7),
];

/// ISA IRQs for the UARTs first (COM1's and COM2's), for the I2C
/// controllers first, then for either: the inputs no legacy device, the
/// SCI, eMMC or GPIO owns.
const UART_IRQS: [u8; 2] = [4, 3];
const I2C_IRQS: [u8; 2] = [10, 11];
const SHARED_IRQS: [u8; 3] = [6, 14, 15];

/// The IOAPIC inputs (ISA IRQs) [`initialize`] routes the FCH's internal
/// sources to: the fixed ones, and an input of its own for each enabled
/// UART and I2C controller, whose edge interrupts can't share one. The
/// others are unrouted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Routes {
    controllers: heapless::Vec<(u8, u8), 10>,
}

/// More controllers are enabled than there are free ISA IRQs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TooManyControllers;

impl Routes {
    /// Routes for the enabled `uarts` (0-3) and `i2c` controllers (0-5).
    pub fn new(
        uarts: impl IntoIterator<Item = u8>,
        i2c: impl IntoIterator<Item = u8>,
    ) -> Result<Self, TooManyControllers> {
        let mut shared = SHARED_IRQS.into_iter();
        let mut controllers = heapless::Vec::new();
        let mut assign = |sources: &[u8], enabled: &mut dyn Iterator<Item = u8>, own: &[u8]| {
            let mut own = own.iter().copied();
            for index in enabled {
                let source = *sources.get(usize::from(index)).ok_or(TooManyControllers)?;
                if controllers.iter().any(|&(s, _)| s == source) {
                    continue;
                }
                let irq = own
                    .next()
                    .or_else(|| shared.next())
                    .ok_or(TooManyControllers)?;
                controllers
                    .push((source, irq))
                    .map_err(|_| TooManyControllers)?;
            }
            Ok(())
        };
        assign(&irq::UART, &mut uarts.into_iter(), &UART_IRQS)?;
        assign(&irq::I2C, &mut i2c.into_iter(), &I2C_IRQS)?;
        Ok(Self { controllers })
    }

    /// The ISA IRQ of the source with PCI_INTR index `index`, if routed.
    pub fn irq(&self, index: u8) -> Option<u8> {
        FIXED_IRQS
            .iter()
            .chain(&self.controllers)
            .find(|&&(source, _)| source == index)
            .map(|&(_, irq)| irq)
    }

    /// Every route [`initialize`] programs, unrouted sources included.
    fn all(&self) -> impl Iterator<Item = (u8, u8)> + '_ {
        let controller = |source: u8| (source, self.irq(source).unwrap_or(irq::NC));
        FIXED_IRQS
            .iter()
            .copied()
            .chain(irq::I2C.into_iter().map(controller))
            .chain(irq::UART.into_iter().map(controller))
    }
}
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
/// ACPI ports/SMBus must already be decoded. The internal sources get
/// `routes`, which the ACPI tables describe.
/// Bank0 RTC accesses follow native C and enable NMI; install exception/NMI
/// handling before this step. No calendar/checksum update is performed.
pub fn initialize<H: FchHw>(fch: &H, espi: bool, legacy_sd: bool, routes: &Routes) {
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
    for (index, apic) in routes.all() {
        route(irq::index(index, irq::Mode::Apic)).set(apic);
    }
    if !legacy_sd {
        legacy_sd_off(acpi);
    }
}
