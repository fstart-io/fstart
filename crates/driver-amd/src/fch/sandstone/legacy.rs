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
    EspiInterruptMask [
        BIT1 OFFSET(1) NUMBITS(1) [],
        BIT12 OFFSET(12) NUMBITS(1) []
    ]
];
register_bitfields![u8,
    RtcB [DST_EN OFFSET(0) NUMBITS(1) []],
    RtcA [BIT4 OFFSET(4) NUMBITS(1) []],
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
    /// The PM1 status bits 8 (power button) and 10 (RTC) of the high byte.
    Pm1StatusHigh [
        PWRBTN OFFSET(0) NUMBITS(1) [],
        RTC OFFSET(2) NUMBITS(1) [],
        WAKE OFFSET(6) NUMBITS(1) []
    ]
];
const PM_ESPI_INTR_MASK: crate::fch::AcpiReg<u32, EspiInterruptMask::Register> = pm::at(0x40);
/// The PCI_INTR index/data pair.
const PCI_INTR_INDEX: u16 = 0xc00;
/// The PM1 status high byte of Picasso's ACPI block at 0x400.
const PM1_STATUS_HIGH: u16 = 0x401;

/// PCI_INTR indexes of the FCH's internal interrupt sources.
pub mod source {
    /// ACPI fixed-event and GPE system-control interrupt.
    pub const SCI: u8 = 0x10;
    /// Fixed-address eMMC SDHCI host.
    pub const EMMC: u8 = 0x43;
    /// The GPIO controller.
    pub const GPIO: u8 = 0x62;
    /// I2C controllers 0-5.
    pub const I2C: [u8; 6] = [0x70, 0x71, 0x72, 0x73, 0x76, 0x77];
    /// UARTs 0-3.
    pub const UART: [u8; 4] = [0x74, 0x75, 0x78, 0x79];
}

/// PCI_INTR's value for a source that reaches no IOAPIC input.
const UNROUTED: u8 = 0x1f;

/// The routes of the sources every board has: INTA-D, the SCI, SD, SDIO,
/// eMMC and the GPIO controller.
const FIXED_IRQS: [(u8, u8); 9] = [
    (0, 0x10),
    (1, 0x11),
    (2, 0x12),
    (3, 0x13),
    (source::SCI, crate::fch::SCI_IRQ),
    (0x17, 0x10),
    (0x1a, 0x10),
    (source::EMMC, 5),
    (source::GPIO, 7),
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
        assign(&source::UART, &mut uarts.into_iter(), &UART_IRQS)?;
        assign(&source::I2C, &mut i2c.into_iter(), &I2C_IRQS)?;
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
        let controller = |source: u8| (source, self.irq(source).unwrap_or(UNROUTED));
        FIXED_IRQS
            .iter()
            .copied()
            .chain(source::I2C.into_iter().map(controller))
            .chain(source::UART.into_iter().map(controller))
    }
}
/// Disable ROM sharing BEFORE the board changes GPIO67 to the DMIC mux.
/// BYTE PCI access is intentional: adjacent control bytes are not touched.
pub fn disable_rom_sharing(bus: &impl Bus) {
    bus.reg(lpc::PCI_CONTROL)
        .modify(PciControl::VW_ROM_SHARING_EN::CLEAR + PciControl::EXT_ROM_SHARING_EN::CLEAR);
}
pub fn lpc_init(bus: &impl Bus) {
    bus.reg(lpc::RA4).modify(LpcA4::BIT0::SET);
    bus.reg(lpc::CLOCK_CONTROL).modify(ClockControl::BIT31::SET);
}
/// ACPI ports/SMBus must already be decoded. The internal sources get
/// `routes`, which the ACPI tables describe.
/// Bank0 RTC accesses follow native C and enable NMI; install exception/NMI
/// handling before this step. No calendar/checksum update is performed.
pub fn initialize<B: Bus>(fch: &Fch<B>, espi: bool, legacy_sd: bool, routes: &Routes) {
    let bus = fch.bus();
    if espi {
        fch.reg(PM_ESPI_INTR_MASK)
            .modify(EspiInterruptMask::BIT1::CLEAR + EspiInterruptMask::BIT12::CLEAR);
    }
    let pm0a = indexed_io::<B, Pm0a::Register>(bus, PM_INDEX, PM_0A);
    pm0a.modify(Pm0a::BIT3::SET);
    pm0a.modify(Pm0a::BIT4::SET);
    indexed_io::<B, PmC0::Register>(bus, PM_INDEX, PM_C0).modify(PmC0::CLEAR_RESET_STATUS::SET);
    bus.reg(Reg::<u8>::io(SMBUS_ASF_IO_BASE + 0x14)).set(0); // SMBus autopoll off
    indexed_io::<B, RtcB::Register>(bus, RTC_INDEX, 0x0b).modify(RtcB::DST_EN::CLEAR);
    indexed_io::<B, RtcA::Register>(bus, RTC_INDEX, 0x0a).modify(RtcA::BIT4::CLEAR);
    let irq = |index| indexed_io::<B, ()>(bus, PCI_INTR_INDEX, index);
    indexed_io::<B, IntrMiscMap::Register>(bus, PCI_INTR_INDEX, 8).modify(
        IntrMiscMap::IRQ1_ROUTED::SET
            + IntrMiscMap::IRQ12_ROUTED::SET
            + IntrMiscMap::IRQ14::Routed
            + IntrMiscMap::IRQ15::Routed,
    );
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
    for (index, apic) in routes.all() {
        irq(index | 0x80).set(apic);
    }
    if !legacy_sd {
        fch.legacy_sd_off();
    }
}
