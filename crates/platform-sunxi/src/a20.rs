//! Allwinner A20 fixed SRAM and DRAM stage flow.

use fstart_core::handoff::HANDOFF_MAX_SIZE;
use fstart_driver_sunxi::a20_ccu::{A20CcuConfig, A20Uart};
use fstart_driver_sunxi::a20_dramc::A20DramcConfig;
use fstart_driver_sunxi::a20_mmc::{A20MmcConfig, A20MmcController};
use fstart_driver_sunxi::a20_spi::{A20_SPI_DEFAULT_FREQ, A20_SPI_DEFAULT_SIZE, A20SpiConfig};
use serde::Serialize;

/// A20 physical DRAM window. The controller detects the actual populated size.
pub const A20_DRAM_BASE: u64 = 0x4000_0000;
pub const A20_DRAM_END: u64 = 0x8000_0000;

/// Closed, board-owned A20 policy consumed by the handwritten flow.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(deny_unknown_fields)]
pub struct A20Config {
    pub ccu: A20CcuConfig,
    pub dram: A20DramcConfig,
    pub mmc0: A20MmcConfig,
    pub spi: A20SpiConfig,
    pub mainstage_load_addr: u64,
    pub handoff_addr: u64,
    /// Kernel command line patched into the FDT chosen node for Linux boot.
    pub bootargs: &'static str,
    /// DRAM scratch address the patched FDT is written to (0 = no patching).
    pub fdt_dst_addr: u64,
}

impl A20Config {
    /// Start an A20 board policy with fixed clock and MMC0 policies.
    #[must_use]
    pub const fn new(dram: A20DramcConfig, mainstage_load_addr: u64, handoff_addr: u64) -> Self {
        Self {
            ccu: A20CcuConfig::new(A20Uart::Uart0),
            dram,
            mmc0: A20MmcConfig::new(A20MmcController::Mmc0),
            spi: A20SpiConfig::new(A20_SPI_DEFAULT_FREQ, A20_SPI_DEFAULT_SIZE),
            mainstage_load_addr,
            handoff_addr,
            bootargs: "",
            fdt_dst_addr: 0,
        }
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

    /// Replace the fixed A20 clock policy. Present for uniform POD builders.
    #[must_use]
    pub const fn ccu(mut self, ccu: A20CcuConfig) -> Self {
        self.ccu = ccu;
        self
    }

    /// Select the boot MMC controller. The initial A20 flow permits MMC0 only.
    #[must_use]
    pub const fn mmc0(mut self, mmc0: A20MmcConfig) -> Self {
        self.mmc0 = mmc0;
        self
    }

    /// Replace the SPI0 NOR flash policy used on SPI boot.
    #[must_use]
    pub const fn spi(mut self, spi: A20SpiConfig) -> Self {
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

    /// Validate the static A20 policy.
    #[must_use]
    pub const fn build(self) -> Self {
        if !in_a20_dram(self.mainstage_load_addr) {
            panic!("A20 mainstage load address must be in DRAM");
        }
        if !in_a20_dram(self.handoff_addr)
            || !in_a20_dram_end(self.handoff_addr, HANDOFF_MAX_SIZE as u64)
        {
            panic!("A20 handoff must fit in DRAM");
        }
        match self.handoff_addr.checked_add(HANDOFF_MAX_SIZE as u64) {
            Some(end) if end <= self.mainstage_load_addr => {}
            _ => panic!("A20 handoff must not overlap mainstage load address"),
        }
        match self.mmc0.controller {
            A20MmcController::Mmc0 => {}
        }
        if self.dram.clock < 24 || self.dram.clock > 600 {
            panic!("A20 DRAM clock is unusable");
        }
        if self.dram.cas < 4 || self.dram.cas > 10 {
            panic!("A20 DRAM CAS is unusable");
        }
        if self.dram.zq == 0 || self.dram.tpr0 == 0 || self.dram.tpr1 == 0 || self.dram.tpr2 == 0 {
            panic!("A20 DRAM timing is incomplete");
        }
        Self {
            spi: self.spi.build(),
            ..self
        }
    }
}

const fn in_a20_dram(addr: u64) -> bool {
    addr >= A20_DRAM_BASE && addr < A20_DRAM_END
}

const fn in_a20_dram_end(addr: u64, size: u64) -> bool {
    match addr.checked_add(size) {
        Some(end) => end <= A20_DRAM_END,
        None => false,
    }
}

#[cfg(all(feature = "stage", feature = "a20", target_arch = "arm"))]
mod stage {
    use super::*;
    use fstart_core::services::{MemoryInit, ServiceError};
    use fstart_driver_sunxi::a20_ccu::{A20_EGON_MMC_OFFSET, A20_SRAM_BASE, A20Ccu};
    use fstart_driver_sunxi::a20_dramc::A20Dramc;
    use fstart_driver_sunxi::a20_mmc::A20Mmc;
    use fstart_driver_sunxi::a20_spi::{A20Spi, A20SpiFlash};
    use fstart_driver_sunxi::spi_nor::SpiNorFlash;
    use fstart_driver_uart::ns16550::{Ns16550, Ns16550Config};
    use fstart_log::StaticConsole;

    use crate::egon::{BootDevice, boot_device_at};
    use crate::stage::{BootMedia, DramStage};

    /// The A20's boot media: eGON MMC0 or SPI NOR.
    pub type A20BootMedia = BootMedia<A20Mmc, A20SpiFlash>;

    /// Open the medium the BROM booted from, and the image's offset on it.
    fn boot_media(config: &'static A20Config) -> Result<(A20BootMedia, u64), ServiceError> {
        match boot_device_at(A20_SRAM_BASE as usize) {
            BootDevice::Mmc0 => {
                let mut mmc0 = A20Mmc::new_from_config(&config.mmc0);
                mmc0.init()?;
                Ok((BootMedia::Mmc(mmc0), A20_EGON_MMC_OFFSET))
            }
            BootDevice::Spi => {
                let mut spi = A20Spi::new_from_config(&config.spi);
                spi.init()?;
                let flash = SpiNorFlash::from_controller(spi, config.spi.flash_size);
                flash.probe()?;
                Ok((BootMedia::Spi(flash), 0))
            }
            _ => {
                fstart_log::error!("a20: unsupported eGON boot device");
                Err(ServiceError::NotSupported)
            }
        }
    }

    /// The A20 SRAM bootblock: clocks and UART0's pins (the CCU), the
    /// console, DRAM, then the DRAM stage from the BROM's boot medium. Boards
    /// have no work here; their bootblock `main` calls this.
    #[inline(always)]
    pub fn bootblock(
        config: &'static A20Config,
        slot: &'static StaticConsole<Ns16550>,
        console: Ns16550Config,
    ) -> ! {
        if run_bootblock(config, slot, console).is_err() {
            fstart_log::error!("a20 bootblock failed");
        }
        fstart_arch::halt()
    }

    #[inline(always)]
    fn run_bootblock(
        config: &'static A20Config,
        slot: &'static StaticConsole<Ns16550>,
        console: Ns16550Config,
    ) -> Result<core::convert::Infallible, ServiceError> {
        fstart_stage::anchor::initial_stage();
        A20Ccu::new_from_config(&config.ccu).init_early()?;
        crate::stage::console(slot, console)?;
        fstart_log::info!("a20: UART0 console ready");
        let dram = A20Dramc::new_from_config(&config.dram).bring_up()?;
        let (media, media_base) = boot_media(config)?;
        crate::stage::enter_dram_stage(
            &media,
            media_base,
            dram,
            config.mainstage_load_addr,
            config.handoff_addr,
        )
    }

    /// The A20 DRAM stage: console, the verified boot media, then the
    /// payload. Boards have no work here; their DRAM-stage `main` calls this.
    pub fn main_stage(
        config: &'static A20Config,
        slot: &'static StaticConsole<Ns16550>,
        console: Ns16550Config,
        handoff: usize,
    ) -> ! {
        let handoff = crate::stage::handoff(handoff, config.handoff_addr);
        let Ok(console) = crate::stage::console(slot, console) else {
            fstart_arch::halt()
        };
        let Ok((media, media_base)) = boot_media(config) else {
            fstart_stage::fail("a20 boot media")
        };
        DramStage::new("a20", console, media, media_base, &handoff, A20_DRAM_BASE).boot(
            config.handoff_addr,
            config.bootargs,
            config.fdt_dst_addr,
        )
    }
}

#[cfg(all(feature = "stage", feature = "a20", target_arch = "arm"))]
pub use stage::{A20BootMedia, bootblock, main_stage};

#[cfg(test)]
mod tests {
    use super::*;

    const DRAM: A20DramcConfig = A20DramcConfig::new(
        432,
        0,
        123,
        false,
        6,
        0x3092_6692,
        0x1090,
        0x0001_a0c8,
        0,
        0,
        4,
        0,
        0,
        0,
        false,
    );
    const VALID: A20Config = A20Config::new(DRAM, 0x4100_0000, 0x40ff_f000).build();

    #[test]
    #[should_panic(expected = "A20 mainstage load address must be in DRAM")]
    fn config_rejects_non_dram_mainstage() {
        let _ = VALID;
        let _ = A20Config::new(DRAM, 0, 0x40ff_f000).build();
    }
}
