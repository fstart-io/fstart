// SPDX-License-Identifier: MIT OR Apache-2.0
//! Family 17h FCH pre-console mechanisms shared by Picasso and Zeppelin:
//! LPC decodes and SPI BAR, SMBus, legacy/CF9 decode and the power-failure
//! state.
use super::Decode;
use fstart_pci::{ConfigRegionAccess, PciAddress};
use tock_registers::{
    LocalRegisterCopy,
    interfaces::{Readable, Writeable},
    register_bitfields,
    registers::ReadWrite,
};

/// SPI controller/eSPI aperture of every Family 17h FCH.
pub const SPI_BASE: u32 = 0xfec10000;

pub trait Registers {
    fn lpc_read32(&mut self, offset: u16) -> u32;
    fn lpc_write32(&mut self, offset: u16, value: u32);
    fn pm_read8(&mut self, offset: u16) -> u8;
    fn pm_write8(&mut self, offset: u16, value: u8);
    fn pm_read32(&mut self, offset: u16) -> u32;
    fn pm_write32(&mut self, offset: u16, value: u32);
    fn smbus_write8(&mut self, asf: bool, offset: u16, value: u8);
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Unavailable,
    WrongVendor,
    WrongSpiBase,
    SpiBusy,
}
pub(crate) fn lpc_present(io: &mut impl Registers) -> Result<(), Error> {
    let id = io.lpc_read32(0);
    if id == u32::MAX {
        return Err(Error::Unavailable);
    }
    if id & 0xffff != 0x1022 {
        return Err(Error::WrongVendor);
    }
    Ok(())
}
/// Enable LPC, clear its legacy decodes/ranges, establish the fixed SPI/eSPI
/// aperture. Call BEFORE new cold eSPI configuration, not over an active link.
/// No SPI mode/clock changes here. Existing BAR low-byte flags are retained.
pub fn prepare_lpc(io: &mut impl Registers) -> Result<(), Error> {
    lpc_present(io)?;
    let gating = io.pm_read8(0xec);
    let decode = io.lpc_read32(0x48);
    let bar = io.lpc_read32(0xa0);
    if gating == u8::MAX || decode == u32::MAX || bar == u32::MAX {
        return Err(Error::Unavailable);
    }
    io.pm_write8(0xec, gating | 1);
    io.lpc_write32(0x44, 0);
    io.lpc_write32(0x48, decode & 0xff80);
    io.lpc_write32(0x44, 0); // native repeated write retained
    io.lpc_write32(0x4c, 0);
    io.lpc_write32(0x60, 0);
    io.lpc_write32(0x90, 0);
    io.lpc_write32(0xa0, (bar & 0xff) | SPI_BASE);
    Ok(())
}
/// Decode POST codes on port 0x80 to LPC (D14F3x48 bit 21).
pub fn enable_port80(io: &mut impl Registers) {
    let decode = io.lpc_read32(0x48);
    io.lpc_write32(0x48, decode | (1 << 21));
}
/// Native nominal400-kHz divider41 (66MHz/(400kHz*4), integer truncation).
/// This preserves native timing; it does not attest a measured400-kHz ceiling.
pub fn initialize_smbus(io: &mut impl Registers) -> Result<(), Error> {
    let decode = io.pm_read32(0);
    if decode == u32::MAX {
        return Err(Error::Unavailable);
    }
    io.pm_write32(0, (decode & !0xff00) | 0xb00 | (1 << 4));
    io.smbus_write8(false, 0x0e, 41);
    io.smbus_write8(false, 0, 0xff);
    io.smbus_write8(false, 1, 0x1f);
    io.smbus_write8(true, 0, 0xff);
    io.smbus_write8(true, 1, 0x1f);
    Ok(())
}
fn modify_decode(
    io: &mut impl Registers,
    field: tock_registers::fields::FieldValue<u32, Decode::Register>,
) {
    let mut decode = LocalRegisterCopy::<u32, Decode::Register>::new(io.pm_read32(0));
    decode.modify(field);
    io.pm_write32(0, decode.get());
}
/// fch_enable_cf9_io: the CF9 reset port.
pub fn enable_cf9_io(io: &mut impl Registers) {
    modify_decode(io, Decode::CF9::SET);
}
/// fch_enable_legacy_io: the legacy I/O ranges (PIC, PIT, RTC, ...).
pub fn enable_legacy_io(io: &mut impl Registers) {
    modify_decode(io, Decode::LEGACY::SET);
}

register_bitfields![u8,
    /// PMx5B: the setting goes into the low nibble, hardware copies it up.
    RtcShadow [
        POWER_FAILURE OFFSET(0) NUMBITS(2) [Off = 0, On = 1, Previous = 3],
        /// Has to be written as 1.
        POWER_STATE OFFSET(2) NUMBITS(1) []
    ]
];
/// What the system does when AC power returns (board policy).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerFailure {
    Off,
    On,
    Previous,
}
/// pm_set_power_failure_state.
pub fn set_power_failure_state(io: &mut impl Registers, state: PowerFailure) {
    let policy = match state {
        PowerFailure::Off => RtcShadow::POWER_FAILURE::Off,
        PowerFailure::On => RtcShadow::POWER_FAILURE::On,
        PowerFailure::Previous => RtcShadow::POWER_FAILURE::Previous,
    };
    io.pm_write8(0x5b, (policy + RtcShadow::POWER_STATE::SET).value);
}

pub struct Native<P> {
    pci: P,
    _exclusive: core::marker::PhantomData<*mut ()>,
}
impl<P: ConfigRegionAccess> Native<P> {
    /// # Safety
    /// Decoded UC ACPIMMIO + PCI D14F3, exclusive ownership. SPI access
    /// additionally requires SPI_BASE mapped UC, PSP/AP/SMM/EC exclusion and
    /// no ROM Armor. Do not prepare LPC over a live PSP/EC connection.
    pub unsafe fn new(pci: P) -> Self {
        Self {
            pci,
            _exclusive: core::marker::PhantomData,
        }
    }
}
pub(crate) fn rw8(base: usize, offset: u16) -> &'static ReadWrite<u8> {
    assert!(offset < 256);
    unsafe { &*((base + usize::from(offset)) as *const ReadWrite<u8>) }
}
pub(crate) fn rw32(base: usize, offset: u16) -> &'static ReadWrite<u32> {
    assert!(offset < 256 && offset & 3 == 0);
    unsafe { &*((base + usize::from(offset)) as *const ReadWrite<u32>) }
}
impl<P: ConfigRegionAccess> Registers for Native<P> {
    fn lpc_read32(&mut self, offset: u16) -> u32 {
        unsafe { self.pci.read(PciAddress::new(0, 0, 0x14, 3), offset) }
    }
    fn lpc_write32(&mut self, offset: u16, value: u32) {
        unsafe {
            self.pci
                .write(PciAddress::new(0, 0, 0x14, 3), offset, value);
        }
    }
    fn pm_read8(&mut self, offset: u16) -> u8 {
        rw8(crate::ACPI_MMIO + 0x300, offset).get()
    }
    fn pm_write8(&mut self, offset: u16, value: u8) {
        rw8(crate::ACPI_MMIO + 0x300, offset).set(value);
    }
    fn pm_read32(&mut self, offset: u16) -> u32 {
        rw32(crate::ACPI_MMIO + 0x300, offset).get()
    }
    fn pm_write32(&mut self, offset: u16, value: u32) {
        rw32(crate::ACPI_MMIO + 0x300, offset).set(value);
    }
    fn smbus_write8(&mut self, asf: bool, offset: u16, value: u8) {
        rw8(crate::ACPI_MMIO + if asf { 0x900 } else { 0xa00 }, offset).set(value);
    }
}
