// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native Skylake irq.c and common LPC/ITSS interrupt routing.
//! Device IRQs are programmed BEFORE root-port function coalescing. PIC
//! INTLINEs are assigned from the real enumerator AFTER bus assignment.

use super::{
    gpio::MiscConfig,
    pcr::{self, Pcr, Port},
    pmc::AcpiControl,
};
use crate::southbridge::pirq::{PciPin, valid_pirq_route};
use fstart_arch::x86::ioapic::IoApic;
use fstart_pci::{EcamDevice, PCI_INTERRUPT_LINE, PCI_INTERRUPT_PIN, PciEcam, devfn};
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

pub const IOAPIC_BASE: usize = 0xfec0_0000;
pub const IOAPIC_ENTRIES: u16 = 120;
pub const IOAPIC_ID: u8 = 2;
const GPIO_PORTS: [Port; 4] = [Port::Gpio0, Port::Gpio1, Port::Gpio2, Port::Gpio3];
register_bitfields![u32,
    DeviceControl [
        ACPI_ENABLE OFFSET(1) NUMBITS(1) [],
        PIN OFFSET(8) NUMBITS(4) [],
        ACPI_IRQ OFFSET(12) NUMBITS(8) [],
        PCI_IRQ OFFSET(20) NUMBITS(8) []
    ],
    HdaControl [ ACPI_IRQ OFFSET(8) NUMBITS(8) [] ],
    RootPortFuse [ PIN OFFSET(24) NUMBITS(4) [] ],
    TcoInterrupt [
        SELECT OFFSET(0) NUMBITS(3) [],
        ENABLE OFFSET(7) NUMBITS(1) []
    ]
];
register_bitfields![u16,
    IoApicControl [
        SELECT OFFSET(0) NUMBITS(8) [],
        ENABLE OFFSET(8) NUMBITS(1) []
    ],
    GeneralInterrupt [ LIMIT_24 OFFSET(9) NUMBITS(1) [] ]
];
#[derive(Debug, Clone, Copy)]
pub struct DeviceInterrupt {
    pub devfn: u8,
    pub pin: PciPin,
    pub gsi: u8,
}
const fn route(slot: u8, function: u8, pin: PciPin, gsi: u8) -> DeviceInterrupt {
    DeviceInterrupt {
        devfn: devfn(slot, function),
        pin,
        gsi,
    }
}
use PciPin::{A, B, C, D};
/// Literal devintconfig/irq.h table. Do not invent additional RP13..24
/// records: those ports retain their native root-port-specific pin handling.
pub const DEVICE_INTERRUPTS: &[DeviceInterrupt] = &[
    route(0x1f, 3, A, 16),
    route(0x1f, 4, A, 16),
    route(0x1f, 6, A, 16),
    route(0x1f, 7, A, 16),
    route(0x1e, 0, A, 20),
    route(0x1e, 1, B, 21),
    route(0x1e, 2, C, 22),
    route(0x1e, 3, D, 23),
    route(0x1e, 4, B, 21),
    route(0x1e, 5, C, 22),
    route(0x1e, 6, D, 23),
    route(0x1d, 0, A, 16),
    route(0x1d, 1, B, 17),
    route(0x1d, 2, C, 18),
    route(0x1d, 3, D, 19),
    route(0x1c, 0, A, 16),
    route(0x1c, 1, B, 17),
    route(0x1c, 2, C, 18),
    route(0x1c, 3, D, 19),
    route(0x1c, 4, A, 16),
    route(0x1c, 5, B, 17),
    route(0x1c, 6, C, 18),
    route(0x1c, 7, D, 19),
    route(0x19, 0, A, 32),
    route(0x19, 1, B, 33),
    route(0x19, 2, C, 34),
    route(0x17, 0, A, 16),
    route(0x16, 0, A, 16),
    route(0x16, 1, B, 17),
    route(0x16, 2, C, 18),
    route(0x16, 3, D, 19),
    route(0x16, 4, A, 16),
    route(0x15, 0, A, 16),
    route(0x15, 1, B, 17),
    route(0x15, 2, C, 18),
    route(0x15, 3, D, 19),
    route(0x14, 0, A, 16),
    route(0x14, 1, B, 17),
    route(0x14, 2, C, 18),
    route(0x14, 3, A, 16),
    route(0x13, 0, A, 20),
];
#[derive(Debug, Clone, Copy)]
pub struct Policy {
    pub gpio_irq: u8,
    pub sci_irq: u8,
    pub tco_irq: u8,
    pub tco_irq_enabled: bool,
    pub pic_routes: [u8; 8],
}
impl Policy {
    pub const NATIVE: Self = Self {
        gpio_irq: 14,
        sci_irq: 9,
        tco_irq: 9,
        tco_irq_enabled: false,
        pic_routes: [11, 10, 11, 11, 11, 11, 11, 11],
    };
    pub fn validate(self) -> Result<(), Error> {
        if !matches!(self.gpio_irq, 14 | 15)
            || select_irq(self.sci_irq).is_none()
            || select_irq(self.tco_irq).is_none()
            || self.pic_routes.iter().any(|r| !valid_pirq_route(*r))
        {
            return Err(Error::Policy);
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Policy,
    Device,
    Readback,
    Pcr(pcr::Error),
}
impl From<pcr::Error> for Error {
    fn from(e: pcr::Error) -> Self {
        Self::Pcr(e)
    }
}
fn select_irq(irq: u8) -> Option<u32> {
    match irq {
        9 => Some(0),
        10 => Some(1),
        11 => Some(2),
        20..=23 => Some(u32::from(irq - 16)),
        _ => None,
    }
}
fn serial_offset(dev: u8) -> Option<u16> {
    match (dev >> 3, dev & 7) {
        (0x15, f @ 0..=3) => Some(0x200 + u16::from(f) * 4),
        (0x19, 2) => Some(0x210),
        (0x19, 1) => Some(0x214),
        (0x1e, 0) => Some(0x218),
        (0x1e, 1) => Some(0x21c),
        (0x19, 0) => Some(0x220),
        (0x1e, 2) => Some(0x224),
        (0x1e, 3) => Some(0x228),
        _ => None,
    }
}
fn program_device(pcr: &Pcr, entry: DeviceInterrupt) -> Result<(), Error> {
    let device = EcamDevice::new(0, entry.devfn >> 3, entry.devfn & 7);
    if !device.is_present() {
        return Ok(());
    }
    device.write8(PCI_INTERRUPT_PIN, entry.pin as u8);
    device.write8(PCI_INTERRUPT_LINE, entry.gsi);
    let values = DeviceControl::PIN.val(entry.pin as u32)
        + DeviceControl::ACPI_IRQ.val(u32::from(entry.gsi))
        + DeviceControl::PCI_IRQ.val(u32::from(entry.gsi));
    if let Some(offset) = serial_offset(entry.devfn) {
        pcr.register::<DeviceControl::Register>(Port::SerialIo, offset)?
            .modify(values);
        return Ok(());
    }
    match (entry.devfn >> 3, entry.devfn & 7) {
        (0x1f, 3) => pcr
            .register::<HdaControl::Register>(Port::Dsp, 0x500)?
            .modify(HdaControl::ACPI_IRQ.val(u32::from(entry.gsi))),
        (0x14, 1) => pcr
            .register::<DeviceControl::Register>(Port::Otg, 0x200)?
            .modify(values + DeviceControl::ACPI_ENABLE::CLEAR),
        (0x13, 0) => pcr
            .register::<DeviceControl::Register>(Port::Ish, 0x200)?
            .modify(values),
        (0x1e, f @ 4..=6) => pcr
            .register::<DeviceControl::Register>(Port::Scs, 0x200 + 4 * u16::from(f - 4))?
            .modify(values),
        (0x1c, _) | (0x1d, 0..=3) => {
            unsafe { device.register::<u32, RootPortFuse::Register>(0xfc) }
                .modify(RootPortFuse::PIN.val(entry.pin as u32))
        }
        _ => {}
    }
    Ok(())
}
/// # Safety
/// Own actual PCH PCR/PCI routing, with interrupts disabled. Called before
/// function mapping; enabled devfns describe pre-coalescing board policy.
/// Reapply board GPIO pad polarities afterwards, as in the native ordering.
pub unsafe fn configure_native(pcr: &Pcr, enabled: &[u8], policy: Policy) -> Result<(), Error> {
    policy.validate()?;
    let pmc = EcamDevice::new(0, 0x1f, 2);
    if pmc.vendor_id() != 0x8086 {
        return Err(Error::Device);
    }
    let mut polarity = [0x00ff_0000u32, 0, 0, 0];
    polarity[usize::from(policy.gpio_irq / 32)] |= 1 << (policy.gpio_irq % 32);
    for &entry in DEVICE_INTERRUPTS {
        polarity[usize::from(entry.gsi / 32)] |= 1 << (entry.gsi % 32);
        if enabled.contains(&entry.devfn) {
            program_device(pcr, entry)?;
        }
    }
    for (index, value) in polarity.into_iter().enumerate() {
        pcr.register::<()>(Port::Itss, 0x3200 + 4 * index as u16)?
            .set(value);
    }
    for port in GPIO_PORTS {
        pcr.register::<MiscConfig::Register>(port, 0x10)?
            .modify(MiscConfig::IRQ15.val(u32::from(policy.gpio_irq == 15)));
    }
    unsafe { pmc.register::<u32, AcpiControl::Register>(0x44) }
        .modify(AcpiControl::SCI_SELECT.val(select_irq(policy.sci_irq).ok_or(Error::Policy)?));
    pcr.register::<TcoInterrupt::Register>(Port::Smbus, 0)?
        .modify(
            TcoInterrupt::SELECT.val(select_irq(policy.tco_irq).ok_or(Error::Policy)?)
                + TcoInterrupt::ENABLE.val(u32::from(policy.tco_irq_enabled)),
        );
    for irq in [policy.sci_irq, policy.tco_irq] {
        let register = pcr.register::<()>(Port::Itss, 0x3200 + 4 * u16::from(irq / 32))?;
        let bit = 1 << (irq % 32);
        register.set((register.get() & !bit) | if irq >= 20 { bit } else { 0 });
    }
    Ok(())
}
/// # Safety
/// Own P2SB, ITSS and the mapped UC IOAPIC window before MP/interrupt delivery.
pub unsafe fn initialize_ioapic(pcr: &Pcr, p2sb: &EcamDevice) -> Result<(), Error> {
    if p2sb.bus() != 0 || p2sb.dev() != 0x1f || p2sb.func() != 1 || p2sb.vendor_id() != 0x8086 {
        return Err(Error::Device);
    }
    let control = unsafe { p2sb.register::<u16, IoApicControl::Register>(0x64) };
    let selector = ((IOAPIC_BASE >> 12) & 255) as u16;
    if control.read(IoApicControl::SELECT) != selector {
        control.modify(IoApicControl::ENABLE::CLEAR);
        control.modify(IoApicControl::SELECT.val(selector));
    }
    control.modify(IoApicControl::ENABLE::SET);
    if !control.matches_all(IoApicControl::SELECT.val(selector) + IoApicControl::ENABLE::SET) {
        return Err(Error::Readback);
    }
    pcr.register16::<GeneralInterrupt::Register>(Port::Itss, 0x31fc)?
        .modify(GeneralInterrupt::LIMIT_24::CLEAR);
    let ioapic = unsafe { IoApic::new(IOAPIC_BASE) };
    if !ioapic.set_redirection_entries(IOAPIC_ENTRIES) {
        return Err(Error::Readback);
    }
    ioapic.set_id(IOAPIC_ID);
    if ioapic.id() != IOAPIC_ID {
        return Err(Error::Readback);
    }
    ioapic.mask_all(); // Legacy ExtINT is armed only after the 8259 is initialized.
    Ok(())
}
/// Native common LPC pch_pirq_init, applied to the canonical enumerator's
/// device snapshot. No second PCI scan and no independent ECAM access suite.
pub fn assign_pic_lines(pcr: &Pcr, pci: &PciEcam, policy: Policy) -> Result<(), Error> {
    policy.validate()?;
    for index in 0..2 {
        let routes = &policy.pic_routes[4 * index..4 * index + 4];
        pcr.register::<()>(Port::Itss, 0x3100 + 4 * index as u16)?
            .set(u32::from_le_bytes(
                routes.try_into().map_err(|_| Error::Policy)?,
            ));
    }
    for address in pci.devices() {
        let device = pci.device(address).ok_or(Error::Device)?;
        if let Some(pin) = PciPin::from_config(device.read8(PCI_INTERRUPT_PIN)) {
            let irq = policy.pic_routes[usize::from(pin.index())];
            device.write8(PCI_INTERRUPT_LINE, if irq & 0x80 != 0 { 0xff } else { irq });
        }
    }
    Ok(())
}
