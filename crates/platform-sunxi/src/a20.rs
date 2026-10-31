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
    use fstart_core::services::{BlockDevice, ServiceError};
    use fstart_driver_sunxi::a20_ccu::{A20_EGON_MMC_OFFSET, A20_SRAM_BASE, A20Ccu};
    use fstart_driver_sunxi::a20_dramc::A20Dramc;
    use fstart_driver_sunxi::a20_mmc::A20Mmc;
    use fstart_driver_sunxi::a20_spi::{A20Spi, A20SpiFlash};
    use fstart_driver_sunxi::spi_nor::SpiNorFlash;
    use fstart_driver_uart::ns16550::{Ns16550, Ns16550Config};
    use fstart_log::StaticConsole;
    use fstart_payload::MainstagePayload;

    use crate::egon::{BootDevice, boot_device_at};

    /// BROM state retained so a Sunxi stage can return to FEL.
    #[repr(C)]
    pub struct FelStash {
        pub sp: u32,
        pub lr: u32,
        pub cpsr: u32,
        pub sctlr: u32,
        pub vbar: u32,
    }

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

    unsafe extern "C" {
        #[link_name = "fstart_sunxi_fel_stash"]
        static FEL_STASH: FelStash;
        fn fstart_return_to_fel(sp: u32, lr: u32) -> !;
    }

    /// Return to the BROM FEL handler saved by the pre-stack entry hook.
    ///
    /// # Safety
    /// Must be called only after the Sunxi entry hook saved valid BROM state.
    pub unsafe fn return_to_fel() -> ! {
        // SAFETY: the pre-stack hook initializes this before Rust is entered.
        unsafe { fstart_return_to_fel(FEL_STASH.sp, FEL_STASH.lr) }
    }

    /// Boot medium selected by the BROM eGON boot device.
    pub enum A20BootMedia {
        Mmc(A20Mmc),
        Spi(A20SpiFlash),
    }

    impl BlockDevice for A20BootMedia {
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
    const A20_SPI_MEDIA_BASE: u64 = 0;

    /// Initialize the BROM-selected boot medium and its image base offset.
    /// Shared by the SRAM bootblock and the DRAM mainstage re-init.
    fn init_boot_media(
        config: &'static super::A20Config,
    ) -> Result<(A20BootMedia, u64), ServiceError> {
        match boot_device_at(A20_SRAM_BASE as usize) {
            BootDevice::Mmc0 => {
                let mut mmc0 = A20Mmc::new_from_config(&config.mmc0);
                mmc0.init()?;
                Ok((A20BootMedia::Mmc(mmc0), A20_EGON_MMC_OFFSET))
            }
            BootDevice::Spi => {
                let mut spi = A20Spi::new_from_config(&config.spi);
                spi.init()?;
                let flash = SpiNorFlash::from_controller(spi, config.spi.flash_size);
                flash.probe()?;
                Ok((A20BootMedia::Spi(flash), A20_SPI_MEDIA_BASE))
            }
            _ => {
                fstart_log::error!("a20: unsupported eGON boot device");
                Err(ServiceError::NotSupported)
            }
        }
    }

    /// State retained by the DRAM-backed A20 mainstage.
    pub struct A20Mainstage {
        console: &'static Ns16550,
        boot_media: A20BootMedia,
        /// Image base offset on the boot medium (8 KiB MMC, 0 SPI).
        #[cfg(feature = "linux")]
        media_base: u64,
        dram_size: u64,
        #[cfg(feature = "linux")]
        config: &'static super::A20Config,
    }

    impl A20Mainstage {
        #[must_use]
        pub const fn dram_size(&self) -> u64 {
            self.dram_size
        }

        #[must_use]
        pub const fn console(&self) -> &Ns16550 {
            self.console
        }

        #[must_use]
        pub const fn boot_media(&self) -> &A20BootMedia {
            &self.boot_media
        }
    }

    /// Block-backed Linux payload launcher for the A20 mainstage.
    ///
    /// The common `LinuxPayload` launcher boots from memory-mapped firmware;
    /// A20 boards load the FFS from MMC, so the platform owns this launcher
    /// behind the same `MainstagePayload` contract.
    #[cfg(feature = "linux")]
    pub struct A20LinuxPayload;

    #[cfg(feature = "linux")]
    impl fstart_payload::MainstagePayload<A20Mainstage> for A20LinuxPayload {
        fn boot(handoff: fstart_payload::Handoff<'_>, devices: A20Mainstage) -> ! {
            let placement = handoff.placement;
            let Some(ram) = handoff.description.ram() else {
                fstart_arch::halt();
            };
            if crate::boot::install_mainstage_policy(
                A20_DRAM_BASE,
                devices.dram_size,
                devices.config.handoff_addr,
                devices.config.fdt_dst_addr,
            )
            .is_err()
            {
                fstart_log::error!("a20: invalid mainstage memory policy");
                fstart_arch::halt();
            }
            let ffs_size = fstart_stage::anchor::image_size();
            let mut boot =
                fstart_stage::fixed_helpers::BlockDeviceLinuxBoot::new(devices.media_base, 0);
            let anchor = fstart_stage::fstart_anchor_bytes();
            if boot.mount(&devices.boot_media, ffs_size, anchor).is_err()
                || boot.verify(&devices.boot_media).is_err()
                || boot.load_kernel(&devices.boot_media).is_err()
                || boot.load_fdt(&devices.boot_media).is_err()
            {
                fstart_log::error!("a20 mainstage: FFS payload load failed");
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
                fstart_log::error!("a20 mainstage: FDT preparation failed");
                fstart_arch::halt();
            }
            let params = boot.boot_params(placement.bootargs);
            fstart_arch::armv7::boot_linux(&params)
        }
    }

    /// Launcher selected independently of additive backend availability.
    #[cfg(fstart_payload = "linux")]
    pub type A20BuildSelectedPayload = A20LinuxPayload;
    #[cfg(fstart_payload = "halt")]
    pub type A20BuildSelectedPayload = fstart_payload::HaltPayload;

    /// The A20 SRAM bootblock: clocks and UART0's pins (the CCU), the
    /// console, DRAM, then the DRAM stage from the BROM's boot medium. Boards
    /// have no work here; their bootblock `main` calls this.
    // Inlined into the board's `main` so its constant console config folds:
    // the SRAM budget has no room for a runtime 64-bit baud divisor.
    #[inline(always)]
    pub fn bootblock(
        config: &'static A20Config,
        slot: &'static StaticConsole<Ns16550>,
        console_config: Ns16550Config,
    ) -> ! {
        if run_bootblock(config, slot, console_config).is_err() {
            fstart_log::error!("a20 bootblock failed");
        }
        fstart_arch::halt()
    }

    #[inline(always)]
    fn run_bootblock(
        config: &'static A20Config,
        slot: &'static StaticConsole<Ns16550>,
        console_config: Ns16550Config,
    ) -> Result<core::convert::Infallible, ServiceError> {
        fstart_stage::anchor::initial_stage();
        let ccu = A20Ccu::new_from_config(&config.ccu);
        ccu.init_early()?;

        let mut console = Ns16550::new(console_config).map_err(|_| ServiceError::HardwareError)?;
        console.init().map_err(|_| ServiceError::HardwareError)?;
        slot.install(console);
        fstart_log::info!("a20: UART0 console ready");

        let mut dramc = A20Dramc::new_from_config(&config.dram);
        let dram_size = dramc.dram_init()?;

        let (boot_media, media_base) = init_boot_media(config)?;
        let entry = crate::boot::load_mainstage(
            &boot_media,
            media_base,
            A20_DRAM_BASE,
            dram_size,
            config.mainstage_load_addr,
            config.handoff_addr,
        )?;
        // SAFETY: `load_mainstage` checked the handoff buffer lies in the
        // trained DRAM and kept it out of the main stage's load window.
        unsafe { fstart_stage::next_stage::write_handoff(dram_size, config.handoff_addr) }
            .map_err(|_| ServiceError::HardwareError)?;
        fstart_arch::armv7::jump_to_with_handoff(entry, config.handoff_addr as usize)
    }

    /// The A20 DRAM stage: console, the verified boot media, then the
    /// payload. Boards have no work here; their DRAM-stage `main` calls this.
    ///
    /// The generic FFS/payload launcher cannot use block-backed media yet;
    /// this retains the initialized devices until that launcher grows its
    /// driver-owned manifest buffer.
    pub fn main_stage(
        config: &'static A20Config,
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

        let Ok((boot_media, media_base)) = init_boot_media(config) else {
            fstart_log::error!("a20 mainstage: boot media init failed");
            fstart_arch::halt();
        };
        if crate::boot::install_mainstage_locator(&boot_media, media_base, &handoff).is_err() {
            fstart_arch::halt();
        }
        let mainstage = A20Mainstage {
            console,
            boot_media,
            #[cfg(feature = "linux")]
            media_base,
            dram_size: handoff.dram_size.get(),
            #[cfg(feature = "linux")]
            config: config,
        };
        fstart_log::info!("a20 mainstage: {} MiB DRAM", mainstage.dram_size() >> 20);
        // The boot medium is a block device: the launcher reads the FFS
        // from it, not from a mapped firmware window.
        let memory = [fstart_core::services::memory_detect::E820Entry::new(
            A20_DRAM_BASE,
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
        A20BuildSelectedPayload::boot(
            fstart_payload::Handoff::new(description, placement),
            mainstage,
        )
    }
}

#[cfg(all(
    feature = "stage",
    feature = "a20",
    feature = "linux",
    target_arch = "arm"
))]
pub use stage::A20LinuxPayload;
#[cfg(all(feature = "stage", feature = "a20", target_arch = "arm"))]
pub use stage::{
    A20BootMedia, A20BuildSelectedPayload, A20Mainstage, FelStash, bootblock, main_stage,
    return_to_fel,
};

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
