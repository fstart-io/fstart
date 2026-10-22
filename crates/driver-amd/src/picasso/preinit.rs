// SPDX-License-Identifier: MIT OR Apache-2.0
//! Picasso LPC/SPI/SMBus pre-console mechanisms.
//! This is not a flash transaction driver or a complete FCH boot flow.
use crate::fch::early::{self, lpc_present, rw32};
pub use crate::fch::early::{Error, Native, SPI_BASE, initialize_smbus, prepare_lpc};
use fstart_pci::ConfigRegionAccess;
use tock_registers::{
    interfaces::{Readable, Writeable},
    registers::ReadWrite,
};
/// The shared early FCH registers plus the SPI controller.
pub trait Registers: early::Registers {
    fn spi_read16(&mut self, offset: u16) -> u16;
    fn spi_write16(&mut self, offset: u16, value: u16);
    fn spi_read32(&mut self, offset: u16) -> u32;
    fn spi_write32(&mut self, offset: u16, value: u32);
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
fn rw16(base: usize, offset: u16) -> &'static ReadWrite<u16> {
    assert!(offset < 256 && offset & 1 == 0);
    unsafe { &*((base + usize::from(offset)) as *const ReadWrite<u16>) }
}
impl<P: ConfigRegionAccess> Registers for Native<P> {
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
}
#[cfg(test)]
mod tests;
