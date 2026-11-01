//! Allwinner H3 fixed SRAM and DRAM stage flow.

use fstart_core::handoff::HANDOFF_MAX_SIZE;
use fstart_driver_sunxi::h3_ccu::H3CcuConfig;
use fstart_driver_sunxi::h3_dramc::H3DramcConfig;
use fstart_driver_sunxi::h3_mmc::{H3MmcConfig, H3MmcController};
use fstart_driver_sunxi::h3_spi::{H3_SPI_DEFAULT_FREQ, H3_SPI_DEFAULT_SIZE, H3SpiConfig};
use serde::Serialize;

/// H3 physical DRAM window. The controller detects the actual populated size.
pub const H3_DRAM_BASE: u64 = 0x4000_0000;
pub const H3_DRAM_END: u64 = 0x8000_0000;

/// Closed, board-owned H3 policy consumed by the handwritten flow.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(deny_unknown_fields)]
pub struct H3Config {
    pub ccu: H3CcuConfig,
    pub dram: H3DramcConfig,
    pub mmc0: H3MmcConfig,
    pub spi: H3SpiConfig,
    pub mainstage_load_addr: u64,
    pub handoff_addr: u64,
    /// SRAM base the BROM loads the eGON image to (0x0 on H3, 0x10000 on H5).
    pub sram_base: u64,
    /// Kernel command line patched into the FDT chosen node for Linux boot.
    pub bootargs: &'static str,
    /// DRAM scratch address the patched FDT is written to (0 = no patching).
    pub fdt_dst_addr: u64,
}

impl H3Config {
    /// Start an H3 board policy with fixed clock and MMC0 policies.
    #[must_use]
    pub const fn new(dram: H3DramcConfig, mainstage_load_addr: u64, handoff_addr: u64) -> Self {
        Self {
            ccu: H3CcuConfig::new(0),
            dram,
            mmc0: H3MmcConfig::new(H3MmcController::Mmc0),
            spi: H3SpiConfig::new(H3_SPI_DEFAULT_FREQ, H3_SPI_DEFAULT_SIZE),
            mainstage_load_addr,
            handoff_addr,
            sram_base: 0,
            bootargs: "",
            fdt_dst_addr: 0,
        }
    }

    /// Set the SRAM base the BROM loads the eGON image to (H5: 0x10000).
    #[must_use]
    pub const fn sram_base(mut self, sram_base: u64) -> Self {
        self.sram_base = sram_base;
        self
    }

    /// Set the Linux kernel command line.
    #[must_use]
    pub const fn bootargs(mut self, bootargs: &'static str) -> Self {
        self.bootargs = bootargs;
        self
    }

    /// Set the DRAM scratch address for the patched Linux FDT.
    #[must_use]
    pub const fn fdt_dst_addr(mut self, fdt_dst_addr: u64) -> Self {
        self.fdt_dst_addr = fdt_dst_addr;
        self
    }

    /// Replace the fixed H3 clock policy. Present for uniform POD builders.
    #[must_use]
    pub const fn ccu(mut self, ccu: H3CcuConfig) -> Self {
        self.ccu = ccu;
        self
    }

    /// Select the boot MMC controller. The initial H3 flow permits MMC0 only.
    #[must_use]
    pub const fn mmc0(mut self, mmc0: H3MmcConfig) -> Self {
        self.mmc0 = mmc0;
        self
    }

    /// Replace the SPI0 NOR flash policy used on SPI boot.
    #[must_use]
    pub const fn spi(mut self, spi: H3SpiConfig) -> Self {
        self.spi = spi;
        self
    }

    /// Set the DRAM mainstage destination.
    #[must_use]
    pub const fn mainstage_load_addr(mut self, mainstage_load_addr: u64) -> Self {
        self.mainstage_load_addr = mainstage_load_addr;
        self
    }

    /// Set the DRAM handoff buffer location.
    #[must_use]
    pub const fn handoff_addr(mut self, handoff_addr: u64) -> Self {
        self.handoff_addr = handoff_addr;
        self
    }

    /// Validate the static H3 policy.
    #[must_use]
    pub const fn build(self) -> Self {
        if !in_h3_dram(self.mainstage_load_addr) {
            panic!("H3 mainstage load address must be in DRAM");
        }
        if !in_h3_dram(self.handoff_addr)
            || !in_h3_dram_end(self.handoff_addr, HANDOFF_MAX_SIZE as u64)
        {
            panic!("H3 handoff must fit in DRAM");
        }
        match self.handoff_addr.checked_add(HANDOFF_MAX_SIZE as u64) {
            Some(end) if end <= self.mainstage_load_addr => {}
            _ => panic!("H3 handoff must not overlap mainstage load address"),
        }
        match self.mmc0.controller {
            H3MmcController::Mmc0 => {}
        }
        // H3 boards train up to ~600 MHz; the H5 variant's known-good
        // policy is 672 MHz (Orange Pi PC2).
        if self.dram.clock < 24 || self.dram.clock > 768 {
            panic!("H3 DRAM clock is unusable");
        }
        if self.dram.zq == 0 {
            panic!("H3 DRAM ZQ calibration is incomplete");
        }
        Self {
            spi: self.spi.build(),
            ..self
        }
    }
}

const fn in_h3_dram(addr: u64) -> bool {
    addr >= H3_DRAM_BASE && addr < H3_DRAM_END
}

const fn in_h3_dram_end(addr: u64, size: u64) -> bool {
    match addr.checked_add(size) {
        Some(end) => end <= H3_DRAM_END,
        None => false,
    }
}

#[cfg(all(
    feature = "stage",
    feature = "h3",
    any(target_arch = "arm", target_arch = "aarch64")
))]
mod stage {
    use super::*;
    use fstart_core::services::{MemoryInit, ServiceError};
    use fstart_driver_sunxi::h3_ccu::{H3_EGON_MMC_OFFSET, H3Ccu};
    use fstart_driver_sunxi::h3_dramc::H3Dramc;
    use fstart_driver_sunxi::h3_mmc::H3Mmc;
    use fstart_driver_sunxi::h3_spi::{H3Spi, H3SpiFlash};
    use fstart_driver_sunxi::spi_nor::SpiNorFlash;
    use fstart_driver_uart::ns16550::{Ns16550, Ns16550Config};
    use fstart_log::StaticConsole;

    use crate::egon::{BootDevice, boot_device_at};
    use crate::stage::{BootMedia, DramStage};

    /// The H3's boot media: eGON MMC0 or SPI NOR.
    pub type H3BootMedia = BootMedia<H3Mmc, H3SpiFlash>;

    /// Open the medium the BROM booted from, and the image's offset on it.
    fn boot_media(config: &'static H3Config) -> Result<(H3BootMedia, u64), ServiceError> {
        match boot_device_at(config.sram_base as usize) {
            BootDevice::Mmc0 => {
                let mut mmc0 = H3Mmc::new_from_config(&config.mmc0);
                mmc0.init()?;
                Ok((BootMedia::Mmc(mmc0), H3_EGON_MMC_OFFSET))
            }
            BootDevice::Spi => {
                let mut spi = H3Spi::new_from_config(&config.spi);
                spi.init()?;
                let flash = SpiNorFlash::from_controller(spi, config.spi.flash_size);
                flash.probe()?;
                Ok((BootMedia::Spi(flash), 0))
            }
            _ => {
                fstart_log::error!("h3: unsupported eGON boot device");
                Err(ServiceError::NotSupported)
            }
        }
    }

    /// The H3 SRAM bootblock: clocks and UART0's pins (the CCU), the
    /// console, DRAM, then the DRAM stage from the BROM's boot medium. Boards
    /// have no work here; their bootblock `main` calls this.
    #[inline(always)]
    pub fn bootblock(
        config: &'static H3Config,
        slot: &'static StaticConsole<Ns16550>,
        console: Ns16550Config,
    ) -> ! {
        if run_bootblock(config, slot, console).is_err() {
            fstart_log::error!("h3 bootblock failed");
        }
        fstart_arch::halt()
    }

    #[inline(always)]
    fn run_bootblock(
        config: &'static H3Config,
        slot: &'static StaticConsole<Ns16550>,
        console: Ns16550Config,
    ) -> Result<core::convert::Infallible, ServiceError> {
        fstart_stage::anchor::initial_stage();
        H3Ccu::new_from_config(&config.ccu).init_early()?;
        crate::stage::console(slot, console)?;
        fstart_log::info!("h3: UART0 console ready");
        let dram = H3Dramc::new_from_config(&config.dram).bring_up()?;
        let (media, media_base) = boot_media(config)?;
        crate::stage::enter_dram_stage(
            &media,
            media_base,
            dram,
            config.mainstage_load_addr,
            config.handoff_addr,
        )
    }

    /// The H3 DRAM stage: console, the verified boot media, then the
    /// payload. Boards have no work here; their DRAM-stage `main` calls this.
    pub fn main_stage(
        config: &'static H3Config,
        slot: &'static StaticConsole<Ns16550>,
        console: Ns16550Config,
        handoff: usize,
    ) -> ! {
        let handoff = crate::stage::handoff(handoff, config.handoff_addr);
        let Ok(console) = crate::stage::console(slot, console) else {
            fstart_arch::halt()
        };
        fstart_log::info!("h3 mainstage: console ready");
        let Ok((media, media_base)) = boot_media(config) else {
            fstart_stage::fail("h3 boot media")
        };
        DramStage::new("h3", console, media, media_base, &handoff, H3_DRAM_BASE).boot(
            config.handoff_addr,
            config.bootargs,
            config.fdt_dst_addr,
        )
    }
}

#[cfg(all(
    feature = "stage",
    feature = "h3",
    any(target_arch = "arm", target_arch = "aarch64")
))]
pub use stage::{H3BootMedia, bootblock, main_stage};

#[cfg(test)]
mod tests {
    use super::*;

    const DRAM: H3DramcConfig = H3DramcConfig::new(432, 3_881_979, false);
    const VALID: H3Config = H3Config::new(DRAM, 0x4100_0000, 0x40ff_f000).build();

    #[test]
    #[should_panic(expected = "H3 mainstage load address must be in DRAM")]
    fn config_rejects_non_dram_mainstage() {
        let _ = VALID;
        let _ = H3Config::new(DRAM, 0, 0x40ff_f000).build();
    }
}
