//! Allwinner D1 (sun20i) fixed SRAM and DRAM stage flow.
//!
//! The D1 BROM loads the eGON bootblock into SRAM at `0x0002_0000` and enters
//! it in RISC-V M-mode (the C906 vendor-CSR setup lives in the
//! `riscv64-sunxi-entry` arch entry). The bootblock brings up clocks, UART0,
//! DDR3, and MMC0, loads the DRAM mainstage from the FFS image, and jumps to
//! it with a serialized handoff.

use fstart_core::handoff::HANDOFF_MAX_SIZE;
use fstart_driver_sunxi::d1_ccu::D1CcuConfig;
use fstart_driver_sunxi::d1_dramc::D1DramcConfig;
use fstart_driver_sunxi::d1_mmc::{D1MmcConfig, D1MmcController};
use serde::Serialize;

/// D1 physical DRAM window.
pub const D1_DRAM_BASE: u64 = 0x4000_0000;
pub const D1_DRAM_END: u64 = 0x8000_0000;

/// Closed, board-owned D1 policy consumed by the handwritten flow.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(deny_unknown_fields)]
pub struct D1Config {
    pub ccu: D1CcuConfig,
    pub dram: D1DramcConfig,
    pub mmc0: D1MmcConfig,
    pub mainstage_load_addr: u64,
    pub handoff_addr: u64,
    /// Kernel command line patched into the FDT chosen node for Linux boot.
    pub bootargs: &'static str,
    /// DRAM scratch address the patched FDT is written to (0 = no patching).
    pub fdt_dst_addr: u64,
}

impl D1Config {
    /// Start a D1 board policy with the fixed UART0 clock policy.
    #[must_use]
    pub const fn new(dram: D1DramcConfig, mainstage_load_addr: u64, handoff_addr: u64) -> Self {
        Self {
            ccu: D1CcuConfig::new(0),
            dram,
            mmc0: D1MmcConfig::new(D1MmcController::Mmc0),
            mainstage_load_addr,
            handoff_addr,
            bootargs: "",
            fdt_dst_addr: 0,
        }
    }

    /// Replace the fixed D1 clock policy. Present for uniform POD builders.
    #[must_use]
    pub const fn ccu(mut self, ccu: D1CcuConfig) -> Self {
        self.ccu = ccu;
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

    /// Validate the policy at build time.
    #[must_use]
    pub const fn build(self) -> Self {
        if !in_d1_dram(self.mainstage_load_addr) {
            panic!("D1 mainstage load address must be in DRAM");
        }
        if !in_d1_dram(self.handoff_addr) {
            panic!("D1 handoff must be in DRAM");
        }
        match self.handoff_addr.checked_add(HANDOFF_MAX_SIZE as u64) {
            Some(end) if end <= self.mainstage_load_addr => {}
            _ => panic!("D1 handoff must not overlap mainstage load address"),
        }
        if self.dram.dram_clk < 24 || self.dram.dram_clk > 1200 {
            panic!("D1 DRAM clock is unusable");
        }
        if self.dram.dram_zq == 0 {
            panic!("D1 DRAM ZQ calibration is incomplete");
        }
        self
    }
}

const fn in_d1_dram(addr: u64) -> bool {
    addr >= D1_DRAM_BASE && addr < D1_DRAM_END
}

#[cfg(all(feature = "stage", feature = "d1", target_arch = "riscv64"))]
mod stage {
    use super::*;
    use fstart_core::services::ServiceError;
    use fstart_driver_sunxi::d1_ccu::{D1_EGON_MMC_OFFSET, D1_SRAM_BASE, D1Ccu};
    use fstart_driver_sunxi::d1_dramc::D1Dramc;
    use fstart_driver_sunxi::d1_mmc::D1Mmc;
    use fstart_driver_uart::ns16550::{Ns16550, Ns16550Config};
    use fstart_log::StaticConsole;

    use crate::egon::{BootDevice, boot_device_at};

    /// State retained by the DRAM-backed D1 mainstage.
    pub struct D1Mainstage {
        console: &'static Ns16550,
        mmc0: D1Mmc,
        dram_size: u64,
    }

    impl D1Mainstage {
        #[must_use]
        pub const fn dram_size(&self) -> u64 {
            self.dram_size
        }

        #[must_use]
        pub const fn console(&self) -> &Ns16550 {
            self.console
        }

        #[must_use]
        pub const fn mmc0(&self) -> &D1Mmc {
            &self.mmc0
        }
    }

    /// The D1 SRAM bootblock: clocks, console, DDR3, MMC, then the DRAM
    /// stage. The CCU routes its own UART pins, so boards have no work here;
    /// their bootblock `main` calls this.
    // Inlined into the board's `main` so its constant console config folds:
    // the SRAM budget has no room for a runtime 64-bit baud divisor.
    #[inline(always)]
    pub fn bootblock(
        config: &'static D1Config,
        slot: &'static StaticConsole<Ns16550>,
        console_config: Ns16550Config,
    ) -> ! {
        let Err(error) = run_bootblock(config, slot, console_config);
        fstart_log::error!("d1 bootblock failed: {}", error as u8);
        fstart_arch::halt()
    }

    #[inline(always)]
    fn run_bootblock(
        config: &'static D1Config,
        slot: &'static StaticConsole<Ns16550>,
        console_config: Ns16550Config,
    ) -> Result<core::convert::Infallible, ServiceError> {
        fstart_stage::anchor::initial_stage();
        let ccu = D1Ccu::new_from_config(&config.ccu);
        ccu.init();

        let mut console = Ns16550::new(console_config).map_err(|_| ServiceError::HardwareError)?;
        console.init().map_err(|_| ServiceError::HardwareError)?;
        slot.install(console);
        fstart_log::info!("d1: UART0 console ready");

        let mut dramc = D1Dramc::new_from_config(&config.dram);
        let dram_size = dramc.dram_init()?;

        let mut mmc0 = D1Mmc::new_from_config(&config.mmc0);
        mmc0.init()?;

        let sram_base = D1_SRAM_BASE as usize;
        if boot_device_at(sram_base) != BootDevice::Mmc0 {
            fstart_log::error!("d1: only eGON MMC0 boot is supported");
            return Err(ServiceError::NotSupported);
        }

        let entry = crate::boot::load_mainstage(
            &mmc0,
            D1_EGON_MMC_OFFSET,
            D1_DRAM_BASE,
            dram_size,
            config.mainstage_load_addr,
            config.handoff_addr,
        )?;
        fstart_stage::next_stage::serialize_handoff(dram_size, config.handoff_addr)
            .map_err(|_| ServiceError::HardwareError)?;
        fstart_arch::riscv64::jump_to_with_handoff(entry, config.handoff_addr as usize)
    }

    /// The D1 DRAM stage. Boards have no work here; their DRAM-stage `main`
    /// calls this.
    ///
    /// ponytail: halt-only — D1 Linux boot needs OpenSBI staging before the
    /// S-mode kernel jump; add the block-backed launcher when a
    /// hardware-validated board needs it.
    pub fn main_stage(
        config: &'static D1Config,
        slot: &'static StaticConsole<Ns16550>,
        console_config: Ns16550Config,
        handoff: usize,
    ) -> ! {
        if handoff != config.handoff_addr as usize {
            fstart_arch::halt();
        }
        // SAFETY: the fixed predecessor reserves this configured buffer and
        // validates it against trained DRAM before jumping here.
        let Some(handoff) = (unsafe { fstart_stage::handoff::try_deserialize(handoff) }) else {
            fstart_arch::halt();
        };
        let mut console = match Ns16550::new(console_config) {
            Ok(console) => console,
            Err(_) => fstart_arch::halt(),
        };
        if console.init().is_err() {
            fstart_arch::halt();
        }
        slot.install(console);
        fstart_log::info!("d1 mainstage: console ready");
        fstart_log::info!("d1 mainstage: {} MiB DRAM", handoff.dram_size >> 20);
        fstart_log::info!("d1 mainstage: ready for payload");
        loop {
            core::hint::spin_loop();
        }
    }
}

#[cfg(all(feature = "stage", feature = "d1", target_arch = "riscv64"))]
pub use stage::{D1Mainstage, bootblock, main_stage};
