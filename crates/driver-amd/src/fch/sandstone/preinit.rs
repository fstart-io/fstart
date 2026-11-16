// SPDX-License-Identifier: MIT OR Apache-2.0
//! Picasso LPC/SPI/SMBus pre-console mechanisms.
//! This is not a flash transaction driver or a complete FCH boot flow.
use crate::fch::{
    FchHw,
    early::lpc_present,
    lpc::{self, RomDmaEcHostControl, SpiBase},
    spi::{self, ReadMode, Speed},
};
pub use crate::fch::{
    SPI_BASE,
    early::{Error, initialize_smbus, prepare_lpc},
};
use crate::window::Window;
use tock_registers::interfaces::{Readable, Writeable};
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
pub fn configure_spi(fch: &impl FchHw, config: SpiConfig) -> Result<(), Error> {
    lpc_present(fch)?;
    let (lpc, mmio) = (fch.lpc(), fch.mmio());
    let mut bar = lpc.at(lpc::SPI_BASE_ADDRESS).extract();
    if bar.get() == u32::MAX {
        return Err(Error::Unavailable);
    }
    if bar.read(SpiBase::BASE) != SPI_BASE >> 8 {
        return Err(Error::WrongSpiBase);
    }
    // Picasso's compiled controller polls SPI_STATUS, not CNTRL0. The
    // shared header's placement of SPI_BUSY under CNTRL0 is misleading.
    let status = mmio.at(spi::STATUS).extract();
    if status.get() == u32::MAX {
        return Err(Error::Unavailable);
    }
    if status.is_set(spi::Status::BUSY) {
        return Err(Error::SpiBusy);
    }
    let mut control = mmio.at(spi::CNTRL0).extract();
    let mut prefetch = lpc.at(lpc::ROM_DMA_EC_HOST_CONTROL).extract();
    let mut burst = mmio.at(spi::SPI100_HOST_PREFETCH).extract();
    let mut enable = mmio.at(spi::SPI100_ENABLE).extract();
    if control.get() == u32::MAX
        || prefetch.get() == u32::MAX
        || burst.get() == u16::MAX
        || enable.get() == u16::MAX
    {
        return Err(Error::Unavailable);
    }
    bar.modify(SpiBase::ROM_ALT_ENABLE::CLEAR + SpiBase::ROM_ENABLE::SET);
    lpc.at(lpc::SPI_BASE_ADDRESS).set(bar.get());
    prefetch.modify(
        RomDmaEcHostControl::SPI_FROM_HOST_PREFETCH_EN::SET
            + RomDmaEcHostControl::SPI_FROM_USB_PREFETCH_EN::SET,
    );
    lpc.at(lpc::ROM_DMA_EC_HOST_CONTROL).set(prefetch.get());
    burst.modify(spi::Spi100HostPrefetch::RD4DW_EN_HOST.val(config.burst_4dw.into()));
    mmio.at(spi::SPI100_HOST_PREFETCH).set(burst.get());
    let mode = config.mode as u32;
    control.modify(
        spi::Cntrl0::READ_MODE_HIGH.val(mode >> 1) + spi::Cntrl0::READ_MODE_LOW.val(mode & 1),
    );
    mmio.at(spi::CNTRL0).set(control.get());
    mmio.at(spi::SPI100_SPEED).set(config.speed_word());
    enable.modify(spi::Spi100Enable::ENABLE::SET);
    mmio.at(spi::SPI100_ENABLE).set(enable.get());
    Ok(())
}
#[cfg(test)]
mod tests;
