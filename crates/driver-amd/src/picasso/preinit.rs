// SPDX-License-Identifier: MIT OR Apache-2.0
//! Picasso LPC/SPI/SMBus pre-console mechanisms.
//! This is not a flash transaction driver or a complete FCH boot flow.
use crate::{
    fch::{Bus, Fch, early::lpc_present, spi},
    lpc::{self, RomDmaEcHostControl, SpiBase},
};
pub use crate::fch::{
    SPI_BASE,
    early::{Error, initialize_smbus, prepare_lpc},
};
use tock_registers::interfaces::{Readable, Writeable};
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
}
/// Native ROM CS/prefetch/4DW/mode/SPI100 sequence. Caller must own SPI against
/// PSP/AP/SMM/EC and establish that ROM Armor does not prohibit host access.
/// An active transaction is rejected before any write, never aborted/retried.
pub fn configure_spi(fch: &Fch<impl Bus>, config: SpiConfig) -> Result<(), Error> {
    lpc_present(fch)?;
    let bus = fch.bus();
    let mut bar = bus.reg(lpc::SPI_BASE_ADDRESS).extract();
    if bar.get() == u32::MAX {
        return Err(Error::Unavailable);
    }
    if bar.read(SpiBase::BASE) != SPI_BASE >> 8 {
        return Err(Error::WrongSpiBase);
    }
    // Picasso's compiled controller polls SPI_STATUS, not CNTRL0. The
    // shared header's placement of SPI_BUSY under CNTRL0 is misleading.
    let status = bus.reg(spi::STATUS).extract();
    if status.get() == u32::MAX {
        return Err(Error::Unavailable);
    }
    if status.is_set(spi::Status::BUSY) {
        return Err(Error::SpiBusy);
    }
    let mut control = bus.reg(spi::CNTRL0).extract();
    let mut prefetch = bus.reg(lpc::ROM_DMA_EC_HOST_CONTROL).extract();
    let mut burst = bus.reg(spi::SPI100_HOST_PREFETCH).extract();
    let mut enable = bus.reg(spi::SPI100_ENABLE).extract();
    if control.get() == u32::MAX
        || prefetch.get() == u32::MAX
        || burst.get() == u16::MAX
        || enable.get() == u16::MAX
    {
        return Err(Error::Unavailable);
    }
    bar.modify(SpiBase::ROM_ALT_ENABLE::CLEAR + SpiBase::ROM_ENABLE::SET);
    bus.reg(lpc::SPI_BASE_ADDRESS).set(bar.get());
    prefetch.modify(
        RomDmaEcHostControl::SPI_FROM_HOST_PREFETCH_EN::SET
            + RomDmaEcHostControl::SPI_FROM_USB_PREFETCH_EN::SET,
    );
    bus.reg(lpc::ROM_DMA_EC_HOST_CONTROL)
        .set(prefetch.get());
    burst.modify(spi::Spi100HostPrefetch::RD4DW_EN_HOST.val(config.burst_4dw.into()));
    bus.reg(spi::SPI100_HOST_PREFETCH).set(burst.get());
    let mode = config.mode as u32;
    control.modify(
        spi::Cntrl0::READ_MODE_HIGH.val(mode >> 1) + spi::Cntrl0::READ_MODE_LOW.val(mode & 1),
    );
    bus.reg(spi::CNTRL0).set(control.get());
    bus.reg(spi::SPI100_SPEED).set(config.speed_word());
    enable.modify(spi::Spi100Enable::ENABLE::SET);
    bus.reg(spi::SPI100_ENABLE).set(enable.get());
    Ok(())
}
#[cfg(test)]
mod tests;
