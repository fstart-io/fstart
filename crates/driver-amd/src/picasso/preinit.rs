// SPDX-License-Identifier: MIT OR Apache-2.0
//! Picasso LPC/SPI/SMBus pre-console mechanisms.
//! This is not a flash transaction driver or a complete FCH boot flow.
use fstart_pci::{ConfigRegionAccess, PciAddress};
use tock_registers::{
    interfaces::{Readable, Writeable},
    registers::ReadWrite,
};
pub const SPI_BASE: u32 = 0xfec10000;
pub trait Registers {
    fn lpc_read32(&mut self, offset: u16) -> u32;
    fn lpc_write32(&mut self, offset: u16, value: u32);
    fn pm_read8(&mut self, offset: u16) -> u8;
    fn pm_write8(&mut self, offset: u16, value: u8);
    fn pm_read32(&mut self, offset: u16) -> u32;
    fn pm_write32(&mut self, offset: u16, value: u32);
    fn spi_read16(&mut self, offset: u16) -> u16;
    fn spi_write16(&mut self, offset: u16, value: u16);
    fn spi_read32(&mut self, offset: u16) -> u32;
    fn spi_write32(&mut self, offset: u16, value: u32);
    fn smbus_write8(&mut self, asf: bool, offset: u16, value: u8);
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Unavailable,
    WrongVendor,
    WrongSpiBase,
    SpiBusy,
}
fn lpc_present(io: &mut impl Registers) -> Result<(), Error> {
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Speed {
    Mhz66 = 0,
    Mhz33 = 1,
    Mhz22 = 2,
    Mhz16 = 3,
    Mhz100 = 4,
    Khz800 = 5,
}
impl Speed {
    const fn rank(self) -> u8 {
        match self {
            Self::Khz800 => 0,
            Self::Mhz16 => 1,
            Self::Mhz22 => 2,
            Self::Mhz33 => 3,
            Self::Mhz66 => 4,
            Self::Mhz100 => 5,
        }
    }
    pub const fn slower(self, other: Self) -> Self {
        if self.rank() <= other.rank() {
            self
        } else {
            other
        }
    }
}
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum ReadMode {
    Normal33 = 0,
    Dual112 = 2,
    Quad114 = 3,
    Dual122 = 4,
    Quad144 = 5,
    Normal66 = 6,
    Fast = 7,
}
#[derive(Debug, Clone, Copy)]
pub struct SpiConfig {
    pub mode: ReadMode,
    pub normal: Speed,
    pub fast: Speed,
    pub alternate: Speed,
    pub tpm: Speed,
    pub burst_4dw: bool,
}
impl SpiConfig {
    /// EFS/board fast-speed override lowers the other speed classes as native
    /// does. Raw EFS values must already have been validated by the caller.
    pub const fn with_fast_override(self, mode: ReadMode, fast: Speed) -> Self {
        let changed = fast as u8 != self.fast as u8;
        Self {
            mode,
            fast,
            normal: if changed {
                self.normal.slower(fast)
            } else {
                self.normal
            },
            alternate: if changed {
                self.alternate.slower(fast)
            } else {
                self.alternate
            },
            tpm: if changed {
                self.tpm.slower(fast)
            } else {
                self.tpm
            },
            ..self
        }
    }
    const fn speed_word(self) -> u16 {
        ((self.normal as u16) << 12)
            | ((self.fast as u16) << 8)
            | ((self.alternate as u16) << 4)
            | self.tpm as u16
    }
    const fn mode_bits(self) -> u32 {
        let mode = self.mode as u32;
        ((mode >> 1) << 29) | ((mode & 1) << 18)
    }
}
/// Native ROM CS/prefetch/4DW/mode/SPI100 sequence. Caller must own SPI against
/// PSP/AP/SMM/EC and establish that ROM Armor does not prohibit host access.
/// An active transaction is rejected before any write, never aborted/retried.
pub fn configure_spi(io: &mut impl Registers, config: SpiConfig) -> Result<(), Error> {
    lpc_present(io)?;
    let bar = io.lpc_read32(0xa0);
    if bar == u32::MAX {
        return Err(Error::Unavailable);
    }
    if bar & !0xff != SPI_BASE {
        return Err(Error::WrongSpiBase);
    }
    // Picasso's compiled controller polls SPI_STATUS, not CNTRL0. The
    // shared header's placement of SPI_BUSY under CNTRL0 is misleading.
    let status = io.spi_read32(0x4c);
    if status == u32::MAX {
        return Err(Error::Unavailable);
    }
    if status & (1 << 31) != 0 {
        return Err(Error::SpiBusy);
    }
    let control = io.spi_read32(0);
    let prefetch = io.lpc_read32(0xb8);
    let burst = io.spi_read16(0x2c);
    let enable = io.spi_read16(0x20);
    if control == u32::MAX || prefetch == u32::MAX || burst == u16::MAX || enable == u16::MAX {
        return Err(Error::Unavailable);
    }
    io.lpc_write32(0xa0, (bar & !3) | 2);
    io.lpc_write32(0xb8, prefetch | (1 << 24) | (1 << 23));
    io.spi_write16(
        0x2c,
        (burst & !(1 << 15)) | if config.burst_4dw { 1 << 15 } else { 0 },
    );
    io.spi_write32(0, (control & !((3 << 29) | (1 << 18))) | config.mode_bits());
    io.spi_write16(0x22, config.speed_word());
    io.spi_write16(0x20, enable | 1);
    Ok(())
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
pub struct Native<P> {
    pci: P,
    _exclusive: core::marker::PhantomData<*mut ()>,
}
impl<P: ConfigRegionAccess> Native<P> {
    /// # Safety
    /// Picasso only, decodedUC ACPIMMIO + PCI D14F3, exclusive ownership. SPI
    /// access additionally requires SPI_BASE mappedUC, PSP/AP/SMM/EC exclusion
    /// and no ROM Armor. Do not prepare LPC over a live PSP/EC connection.
    pub unsafe fn new(pci: P) -> Self {
        Self {
            pci,
            _exclusive: core::marker::PhantomData,
        }
    }
}
fn rw8(base: usize, offset: u16) -> &'static ReadWrite<u8> {
    assert!(offset < 256);
    unsafe { &*((base + usize::from(offset)) as *const ReadWrite<u8>) }
}
fn rw16(base: usize, offset: u16) -> &'static ReadWrite<u16> {
    assert!(offset < 256 && offset & 1 == 0);
    unsafe { &*((base + usize::from(offset)) as *const ReadWrite<u16>) }
}
fn rw32(base: usize, offset: u16) -> &'static ReadWrite<u32> {
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
        rw8(super::ACPI_MMIO + 0x300, offset).get()
    }
    fn pm_write8(&mut self, offset: u16, value: u8) {
        rw8(super::ACPI_MMIO + 0x300, offset).set(value);
    }
    fn pm_read32(&mut self, offset: u16) -> u32 {
        rw32(super::ACPI_MMIO + 0x300, offset).get()
    }
    fn pm_write32(&mut self, offset: u16, value: u32) {
        rw32(super::ACPI_MMIO + 0x300, offset).set(value);
    }
    fn spi_read16(&mut self, offset: u16) -> u16 {
        rw16(SPI_BASE as usize, offset).get()
    }
    fn spi_write16(&mut self, offset: u16, value: u16) {
        rw16(SPI_BASE as usize, offset).set(value);
    }
    fn spi_read32(&mut self, offset: u16) -> u32 {
        rw32(SPI_BASE as usize, offset).get()
    }
    fn spi_write32(&mut self, offset: u16, value: u32) {
        rw32(SPI_BASE as usize, offset).set(value);
    }
    fn smbus_write8(&mut self, asf: bool, offset: u16, value: u8) {
        rw8(super::ACPI_MMIO + if asf { 0x900 } else { 0xa00 }, offset).set(value);
    }
}
#[cfg(test)]
mod tests;
