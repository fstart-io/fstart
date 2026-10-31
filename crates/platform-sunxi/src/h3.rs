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
    use fstart_core::services::{BlockDevice, ServiceError};
    use fstart_driver_sunxi::h3_ccu::{H3_EGON_MMC_OFFSET, H3Ccu};
    use fstart_driver_sunxi::h3_dramc::H3Dramc;
    use fstart_driver_sunxi::h3_mmc::H3Mmc;
    use fstart_driver_sunxi::h3_spi::{H3Spi, H3SpiFlash};
    use fstart_driver_sunxi::spi_nor::SpiNorFlash;
    use fstart_driver_uart::ns16550::{Ns16550, Ns16550Config};
    use fstart_log::StaticConsole;
    use fstart_payload::MainstagePayload;

    use crate::egon::{BootDevice, boot_device_at};

    /// BROM state retained so a Sunxi stage can return to FEL.
    #[cfg(target_arch = "arm")]
    #[repr(C)]
    pub struct FelStash {
        pub sp: u32,
        pub lr: u32,
        pub cpsr: u32,
        pub sctlr: u32,
        pub vbar: u32,
    }

    #[cfg(target_arch = "arm")]
    core::arch::global_asm!(
        r#"
        .section .text.entry
        .global fstart_pre_stack_entry
        .global fstart_return_to_fel
        .global fstart_sunxi_fel_stash
        .arm
fstart_pre_stack_entry:
        ldr r0, =fstart_sunxi_fel_stash
        str sp, [r0, #0]
        str lr, [r0, #4]
        mrs r1, cpsr
        str r1, [r0, #8]
        mrc p15, 0, r1, c1, c0, 0
        str r1, [r0, #12]
        mrc p15, 0, r1, c12, c0, 0
        str r1, [r0, #16]
        bx lr
fstart_return_to_fel:
        mov sp, r0
        mov lr, r1
        ldr r0, =fstart_sunxi_fel_stash
        ldr r1, [r0, #16]
        mcr p15, 0, r1, c12, c0, 0
        ldr r1, [r0, #12]
        mcr p15, 0, r1, c1, c0, 0
        ldr r1, [r0, #8]
        msr cpsr, r1
        bx lr
        .ltorg
        .section .data
        .align 2
fstart_sunxi_fel_stash:
        .space 20
        "#
    );

    #[cfg(target_arch = "arm")]
    unsafe extern "C" {
        #[link_name = "fstart_sunxi_fel_stash"]
        static FEL_STASH: FelStash;
        fn fstart_return_to_fel(sp: u32, lr: u32) -> !;
    }

    /// Return to the BROM FEL handler saved by the pre-stack entry hook.
    ///
    /// AArch64 sunxi FEL return (RMR switch back to AArch32) is not
    /// implemented; the stash is saved by the RMR entry but unused.
    ///
    /// # Safety
    /// Must be called only after the Sunxi entry hook saved valid BROM state.
    #[cfg(target_arch = "arm")]
    pub unsafe fn return_to_fel() -> ! {
        // SAFETY: the pre-stack hook initializes this before Rust is entered.
        unsafe { fstart_return_to_fel(FEL_STASH.sp, FEL_STASH.lr) }
    }

    /// Boot medium selected by the BROM eGON boot device.
    pub enum H3BootMedia {
        Mmc(H3Mmc),
        Spi(H3SpiFlash),
    }

    impl BlockDevice for H3BootMedia {
        fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, ServiceError> {
            match self {
                Self::Mmc(mmc) => mmc.read(offset, buf),
                Self::Spi(flash) => flash.read(offset, buf),
            }
        }

        fn write(&self, offset: u64, buf: &[u8]) -> Result<usize, ServiceError> {
            match self {
                Self::Mmc(mmc) => mmc.write(offset, buf),
                Self::Spi(flash) => flash.write(offset, buf),
            }
        }

        fn size(&self) -> u64 {
            match self {
                Self::Mmc(mmc) => mmc.size(),
                Self::Spi(flash) => flash.size(),
            }
        }

        fn block_size(&self) -> u32 {
            match self {
                Self::Mmc(mmc) => mmc.block_size(),
                Self::Spi(flash) => flash.block_size(),
            }
        }
    }

    /// SPI NOR images start at flash offset 0; eGON MMC images at 8 KiB.
    const H3_SPI_MEDIA_BASE: u64 = 0;

    /// Initialize the BROM-selected boot medium and its image base offset.
    /// Shared by the SRAM bootblock and the DRAM mainstage re-init.
    fn init_boot_media(
        config: &'static super::H3Config,
    ) -> Result<(H3BootMedia, u64), ServiceError> {
        match boot_device_at(config.sram_base as usize) {
            BootDevice::Mmc0 => {
                let mut mmc0 = H3Mmc::new_from_config(&config.mmc0);
                mmc0.init()?;
                Ok((H3BootMedia::Mmc(mmc0), H3_EGON_MMC_OFFSET))
            }
            BootDevice::Spi => {
                let mut spi = H3Spi::new_from_config(&config.spi);
                spi.init()?;
                let flash = SpiNorFlash::from_controller(spi, config.spi.flash_size);
                flash.probe()?;
                Ok((H3BootMedia::Spi(flash), H3_SPI_MEDIA_BASE))
            }
            _ => {
                fstart_log::error!("h3: unsupported eGON boot device");
                Err(ServiceError::NotSupported)
            }
        }
    }

    /// State retained by the DRAM-backed H3 mainstage.
    pub struct H3Mainstage {
        console: &'static Ns16550,
        boot_media: H3BootMedia,
        /// Image base offset on the boot medium (8 KiB MMC, 0 SPI).
        #[cfg(feature = "linux")]
        media_base: u64,
        dram_size: u64,
    }

    impl H3Mainstage {
        #[must_use]
        pub const fn dram_size(&self) -> u64 {
            self.dram_size
        }

        #[must_use]
        pub const fn console(&self) -> &Ns16550 {
            self.console
        }

        #[must_use]
        pub const fn boot_media(&self) -> &H3BootMedia {
            &self.boot_media
        }
    }

    /// Block-backed Linux payload launcher for the H3 mainstage.
    ///
    /// The common `LinuxPayload` launcher boots from memory-mapped firmware;
    /// H3 boards load the FFS from the boot medium, so the platform owns
    /// this launcher behind the same `MainstagePayload` contract.
    #[cfg(all(feature = "linux", target_arch = "arm"))]
    pub struct H3LinuxPayload;

    #[cfg(all(feature = "linux", target_arch = "arm"))]
    impl fstart_payload::MainstagePayload<H3Mainstage> for H3LinuxPayload {
        fn boot(handoff: fstart_payload::Handoff<'_>, devices: H3Mainstage) -> ! {
            let placement = handoff.placement;
            let Some(ram) = handoff.description.ram() else {
                fstart_arch::halt();
            };
            let ffs_size = fstart_stage::anchor::image_size();
            let mut boot =
                fstart_stage::fixed_helpers::BlockDeviceLinuxBoot::new(devices.media_base, 0);
            let anchor = fstart_stage::fstart_anchor_bytes();
            let step = if boot.mount(&devices.boot_media, ffs_size, anchor).is_err() {
                "mount"
            } else if {
                fstart_log::info!("h3 mainstage: verifying FFS");
                boot.verify(&devices.boot_media).is_err()
            } {
                "verify"
            } else if {
                fstart_log::info!("h3 mainstage: loading kernel");
                boot.load_kernel(&devices.boot_media).is_err()
            } {
                "kernel"
            } else if {
                fstart_log::info!("h3 mainstage: loading FDT");
                boot.load_fdt(&devices.boot_media).is_err()
            } {
                "fdt"
            } else {
                ""
            };
            if !step.is_empty() {
                fstart_log::error!("h3 mainstage: FFS payload {} failed", step);
                fstart_arch::halt();
            }
            if placement.device_tree != 0
                && boot
                    .prepare_fdt(
                        placement.device_tree,
                        placement.bootargs,
                        ram.base,
                        ram.size,
                    )
                    .is_err()
            {
                fstart_log::error!("h3 mainstage: FDT preparation failed");
                fstart_arch::halt();
            }
            let params = boot.boot_params(placement.bootargs);
            fstart_arch::armv7::boot_linux(&params)
        }
    }

    /// Block-backed BL31/Linux payload launcher for the H5 mainstage.
    #[cfg(all(feature = "linux", target_arch = "aarch64"))]
    pub struct H5LinuxPayload;

    #[cfg(all(feature = "linux", target_arch = "aarch64"))]
    impl fstart_payload::MainstagePayload<H3Mainstage> for H5LinuxPayload {
        fn boot(handoff: fstart_payload::Handoff<'_>, devices: H3Mainstage) -> ! {
            let placement = handoff.placement;
            let Some(ram) = handoff.description.ram() else {
                fstart_arch::halt();
            };
            let ffs_size = fstart_stage::anchor::image_size();
            let mut boot =
                fstart_stage::fixed_helpers::BlockDeviceLinuxBoot::new(devices.media_base, 0);
            let anchor = fstart_stage::fstart_anchor_bytes();
            let step = if boot.mount(&devices.boot_media, ffs_size, anchor).is_err() {
                "mount"
            } else if {
                fstart_log::info!("h5 mainstage: verifying FFS");
                boot.verify(&devices.boot_media).is_err()
            } {
                "verify"
            } else if {
                fstart_log::info!("h5 mainstage: loading BL31");
                boot.load_firmware(&devices.boot_media).is_err()
            } {
                "BL31"
            } else if {
                fstart_log::info!("h5 mainstage: loading kernel");
                boot.load_kernel(&devices.boot_media).is_err()
            } {
                "kernel"
            } else if {
                fstart_log::info!("h5 mainstage: loading FDT");
                boot.load_fdt(&devices.boot_media).is_err()
            } {
                "fdt"
            } else {
                ""
            };
            if !step.is_empty() {
                fstart_log::error!("h5 mainstage: FFS payload {} failed", step);
                fstart_arch::halt();
            }
            if placement.device_tree != 0
                && boot
                    .prepare_fdt(
                        placement.device_tree,
                        placement.bootargs,
                        ram.base,
                        ram.size,
                    )
                    .is_err()
            {
                fstart_log::error!("h5 mainstage: FDT preparation failed");
                fstart_arch::halt();
            }
            let params = boot.boot_params(placement.bootargs);
            fstart_log::info!(
                "h5 mainstage: booting BL31 at {:#x}, Linux at {:#x}",
                params.fw_addr,
                params.kernel_addr,
            );
            fstart_arch::aarch64::boot_linux(&params)
        }
    }

    /// Launcher selected independently of additive backend availability.
    #[cfg(all(fstart_payload = "linux", target_arch = "arm"))]
    pub type H3BuildSelectedPayload = H3LinuxPayload;
    #[cfg(all(fstart_payload = "linux", target_arch = "aarch64"))]
    pub type H3BuildSelectedPayload = H5LinuxPayload;
    #[cfg(fstart_payload = "halt")]
    pub type H3BuildSelectedPayload = fstart_payload::HaltPayload;

    /// The H3 SRAM bootblock: clocks and UART0's pins (the CCU), the
    /// console, DRAM, then the DRAM stage from the BROM's boot medium. Boards
    /// have no work here; their bootblock `main` calls this.
    // Inlined into the board's `main` so its constant console config folds:
    // the SRAM budget has no room for a runtime 64-bit baud divisor.
    #[inline(always)]
    pub fn bootblock(
        config: &'static H3Config,
        slot: &'static StaticConsole<Ns16550>,
        console_config: Ns16550Config,
    ) -> ! {
        if run_bootblock(config, slot, console_config).is_err() {
            fstart_log::error!("h3 bootblock failed");
        }
        fstart_arch::halt()
    }

    #[inline(always)]
    fn run_bootblock(
        config: &'static H3Config,
        slot: &'static StaticConsole<Ns16550>,
        console_config: Ns16550Config,
    ) -> Result<core::convert::Infallible, ServiceError> {
        fstart_stage::anchor::initial_stage();
        let ccu = H3Ccu::new_from_config(&config.ccu);
        ccu.init_early()?;

        let mut console = Ns16550::new(console_config).map_err(|_| ServiceError::HardwareError)?;
        console.init().map_err(|_| ServiceError::HardwareError)?;
        slot.install(console);
        fstart_log::info!("h3: UART0 console ready");

        use fstart_core::services::MemoryInit as _;
        let mut dramc = H3Dramc::new_from_config(&config.dram);
        let dram = dramc.bring_up()?;

        let (boot_media, media_base) = init_boot_media(config)?;
        let entry = crate::boot::load_mainstage(
            &boot_media,
            media_base,
            dram,
            config.mainstage_load_addr,
            config.handoff_addr,
        )?;
        // SAFETY: `load_mainstage` checked the handoff buffer lies in the
        // trained DRAM and kept it out of the main stage's load window.
        unsafe { fstart_stage::next_stage::write_handoff(dram.size, config.handoff_addr) }
            .map_err(|_| ServiceError::HardwareError)?;
        #[cfg(target_arch = "arm")]
        fstart_arch::armv7::jump_to_with_handoff(entry, config.handoff_addr as usize);
        #[cfg(target_arch = "aarch64")]
        fstart_arch::aarch64::jump_to_with_handoff(entry, config.handoff_addr as usize)
    }

    /// The H3 DRAM stage: console, the verified boot media, then the
    /// payload. Boards have no work here; their DRAM-stage `main` calls this.
    ///
    /// The generic FFS/payload launcher cannot use block-backed media yet;
    /// this retains the initialized devices until that launcher grows its
    /// driver-owned manifest buffer.
    pub fn main_stage(
        config: &'static H3Config,
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
        let console = slot.install(console);
        fstart_log::info!("h3 mainstage: console ready");

        let Ok((boot_media, media_base)) = init_boot_media(config) else {
            fstart_log::error!("h3 mainstage: boot media init failed");
            fstart_arch::halt();
        };
        if crate::boot::install_mainstage_locator(&boot_media, media_base, &handoff).is_err() {
            fstart_arch::halt();
        }
        let mainstage = H3Mainstage {
            console,
            boot_media,
            #[cfg(feature = "linux")]
            media_base,
            dram_size: handoff.dram_size.get(),
        };
        fstart_log::info!("h3 mainstage: {} MiB DRAM", mainstage.dram_size() >> 20);
        #[cfg(fstart_payload = "linux")]
        if crate::boot::install_mainstage_policy(
            H3_DRAM_BASE,
            mainstage.dram_size,
            config.handoff_addr,
            config.fdt_dst_addr,
        )
        .is_err()
        {
            fstart_log::error!("sunxi: invalid mainstage memory policy");
            fstart_arch::halt();
        }
        // The boot medium is a block device: the launcher reads the FFS
        // from it, not from a mapped firmware window.
        let memory = [fstart_core::services::memory_detect::E820Entry::new(
            H3_DRAM_BASE,
            mainstage.dram_size,
            fstart_core::services::memory_detect::E820Kind::Ram,
        )];
        let mut description = fstart_payload::Description::new(&memory, Default::default());
        description.console = Some(mainstage.console);
        #[allow(unused_mut)]
        let mut placement = fstart_payload::Placement::default();
        #[cfg(feature = "linux")]
        {
            placement.device_tree = config.fdt_dst_addr;
            placement.bootargs = config.bootargs;
        }
        H3BuildSelectedPayload::boot(
            fstart_payload::Handoff::new(description, placement),
            mainstage,
        )
    }
}

#[cfg(all(feature = "stage", feature = "h3", target_arch = "arm"))]
pub use stage::{FelStash, return_to_fel};

#[cfg(all(
    feature = "stage",
    feature = "h3",
    feature = "linux",
    target_arch = "arm"
))]
pub use stage::H3LinuxPayload;
#[cfg(all(
    feature = "stage",
    feature = "h3",
    feature = "linux",
    target_arch = "aarch64"
))]
pub use stage::H5LinuxPayload;
#[cfg(all(
    feature = "stage",
    feature = "h3",
    any(target_arch = "arm", target_arch = "aarch64")
))]
pub use stage::{H3BootMedia, H3BuildSelectedPayload, H3Mainstage, bootblock, main_stage};

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
