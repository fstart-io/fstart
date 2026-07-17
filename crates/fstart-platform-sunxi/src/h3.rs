//! Allwinner H3 fixed SRAM and DRAM stage flow.

use fstart_core::handoff::HANDOFF_MAX_SIZE;
use fstart_driver_sunxi::h3_ccu::H3CcuConfig;
use fstart_driver_sunxi::h3_dramc::H3DramcConfig;
use fstart_driver_sunxi::h3_mmc::{H3MmcConfig, H3MmcController};
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
    pub mainstage_load_addr: u64,
    pub handoff_addr: u64,
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
        if self.dram.clock < 24 || self.dram.clock > 600 {
            panic!("H3 DRAM clock is unusable");
        }
        if self.dram.zq == 0 {
            panic!("H3 DRAM ZQ calibration is incomplete");
        }
        self
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

#[cfg(all(feature = "stage", feature = "h3", target_arch = "arm"))]
mod stage {
    use super::*;
    use fstart_core::services::ServiceError;
    use fstart_driver_sunxi::h3_ccu::{H3Ccu, H3_EGON_MMC_OFFSET, H3_SRAM_BASE};
    use fstart_driver_sunxi::h3_dramc::H3Dramc;
    use fstart_driver_sunxi::h3_mmc::H3Mmc;
    use fstart_driver_uart::ns16550::{Ns16550, Ns16550Config};
    use fstart_stage::{payload::MainstagePayload, StageBoard, StageEnvironment};

    #[cfg(feature = "linux")]
    use crate::egon::ffs_total_size_at;
    use crate::egon::{boot_device_at, next_stage_offset_at, next_stage_size_at, BootDevice};

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

    /// Marker for sunxi platform families with handwritten early flows.
    pub trait SunxiPlatform {}

    /// Fixed sunxi SRAM-stage platform contract.
    pub trait SunxiEarlyPlatform: SunxiPlatform {}

    /// Board contract shared by handwritten sunxi early flows.
    pub trait SunxiEarlyBoard: StageBoard {
        type Platform: SunxiEarlyPlatform;
        type Hooks: SunxiEarlyBoardHooks<Self::Platform>;

        fn hooks() -> Result<Self::Hooks, ServiceError>;
    }

    /// Mutable context passed to the H3's board hooks.
    pub struct SunxiEarlyCtx<'a, P: SunxiEarlyPlatform> {
        ccu: &'a H3Ccu,
        dram_size: Option<u64>,
        _platform: core::marker::PhantomData<P>,
    }

    impl<'a, P: SunxiEarlyPlatform> SunxiEarlyCtx<'a, P> {
        fn new(ccu: &'a H3Ccu, dram_size: Option<u64>) -> Self {
            Self {
                ccu,
                dram_size,
                _platform: core::marker::PhantomData,
            }
        }

        #[must_use]
        pub const fn ccu(&self) -> &H3Ccu {
            self.ccu
        }

        #[must_use]
        pub const fn dram_size(&self) -> Option<u64> {
            self.dram_size
        }
    }

    /// Board-defined seams in the fixed H3 SRAM flow.
    pub trait SunxiEarlyBoardHooks<P: SunxiEarlyPlatform> {
        fn before_clock(&mut self, _ctx: &mut SunxiEarlyCtx<P>) -> Result<(), ServiceError> {
            Ok(())
        }

        fn after_clock(&mut self, _ctx: &mut SunxiEarlyCtx<P>) -> Result<(), ServiceError> {
            Ok(())
        }

        fn before_console(&mut self, _ctx: &mut SunxiEarlyCtx<P>) -> Result<(), ServiceError> {
            Ok(())
        }

        fn after_console(&mut self, _ctx: &mut SunxiEarlyCtx<P>) -> Result<(), ServiceError> {
            Ok(())
        }

        fn before_memory(&mut self, _ctx: &mut SunxiEarlyCtx<P>) -> Result<(), ServiceError> {
            Ok(())
        }

        fn after_memory(&mut self, _ctx: &mut SunxiEarlyCtx<P>) -> Result<(), ServiceError> {
            Ok(())
        }

        fn before_handoff(&mut self, _ctx: &mut SunxiEarlyCtx<P>) -> Result<(), ServiceError> {
            Ok(())
        }
    }

    /// Concrete Allwinner H3 platform.
    pub struct H3;

    impl SunxiPlatform for H3 {}
    impl SunxiEarlyPlatform for H3 {}

    /// Board facts required by the fixed H3 flow.
    pub trait H3Board: SunxiEarlyBoard<Platform = H3> {
        type Payload: fstart_stage::payload::MainstagePayload<H3Mainstage>;

        /// Board policy stored in `.rodata`; no pre-DRAM config transforms.
        const CONFIG: &'static H3Config;
        /// Board-selected console device policy, materialized in `.rodata`.
        const CONSOLE_CONFIG: Ns16550Config;
    }

    /// State retained by the DRAM-backed H3 mainstage.
    pub struct H3Mainstage {
        console: Ns16550,
        mmc0: H3Mmc,
        dram_size: u64,
        config: &'static super::H3Config,
    }

    impl H3Mainstage {
        #[must_use]
        pub const fn dram_size(&self) -> u64 {
            self.dram_size
        }

        #[must_use]
        pub const fn console(&self) -> &Ns16550 {
            &self.console
        }

        #[must_use]
        pub const fn mmc0(&self) -> &H3Mmc {
            &self.mmc0
        }
    }

    /// Block-backed Linux payload launcher for the H3 mainstage.
    ///
    /// The common `LinuxPayload` launcher boots from memory-mapped firmware;
    /// H3 boards load the FFS from MMC, so the platform owns this launcher
    /// behind the same `MainstagePayload` contract.
    #[cfg(feature = "linux")]
    pub struct H3LinuxPayload;

    #[cfg(feature = "linux")]
    impl fstart_stage::payload::MainstagePayload<H3Mainstage> for H3LinuxPayload {
        fn boot(devices: H3Mainstage) -> ! {
            let ffs_size = ffs_total_size_at(H3_SRAM_BASE as usize) as usize;
            let mut boot =
                fstart_stage::fixed_helpers::BlockDeviceLinuxBoot::new(H3_EGON_MMC_OFFSET, 0);
            let anchor = fstart_stage::fstart_anchor_bytes();
            let step = if boot.mount(&devices.mmc0, ffs_size, anchor).is_err() {
                "mount"
            } else if {
                fstart_log::info!("h3 mainstage: verifying FFS");
                boot.verify(&devices.mmc0).is_err()
            } {
                "verify"
            } else if {
                fstart_log::info!("h3 mainstage: loading kernel");
                boot.load_kernel(&devices.mmc0).is_err()
            } {
                "kernel"
            } else if {
                fstart_log::info!("h3 mainstage: loading FDT");
                boot.load_fdt(&devices.mmc0).is_err()
            } {
                "fdt"
            } else {
                ""
            };
            if !step.is_empty() {
                fstart_log::error!("h3 mainstage: FFS payload {} failed", step);
                fstart_arch::halt();
            }
            let config = devices.config;
            if config.fdt_dst_addr != 0
                && boot
                    .prepare_fdt(
                        config.fdt_dst_addr,
                        config.bootargs,
                        super::H3_DRAM_BASE,
                        devices.dram_size,
                    )
                    .is_err()
            {
                fstart_log::error!("h3 mainstage: FDT preparation failed");
                fstart_arch::halt();
            }
            let params = boot.boot_params(config.bootargs);
            fstart_arch::armv7::boot_linux(&params)
        }
    }

    /// Payload launcher selected by fbuild features for H3 boards.
    #[cfg(feature = "linux")]
    pub type H3BuildSelectedPayload = H3LinuxPayload;

    /// Payload launcher used when fbuild selected no payload backend.
    #[cfg(not(feature = "linux"))]
    pub type H3BuildSelectedPayload = fstart_stage::payload::HaltPayload;

    impl H3 {
        /// Dispatch the build-selected H3 SRAM or DRAM stage.
        pub fn run_stage<B>(env: StageEnvironment, handoff: usize) -> !
        where
            B: H3Board,
        {
            let _ = env;
            let _ = handoff;
            #[cfg(fstart_stage_env = "car")]
            {
                let Ok(mut hooks) = B::hooks() else {
                    fstart_arch::halt();
                };
                if run_h3_bootblock::<B>(&mut hooks).is_err() {
                    fstart_arch::halt();
                }
                fstart_arch::halt()
            }

            #[cfg(fstart_stage_env = "ram")]
            {
                run_h3_mainstage::<B>(handoff)
            }

            #[cfg(not(any(fstart_stage_env = "car", fstart_stage_env = "ram")))]
            match env {
                StageEnvironment::Car => {
                    let Ok(mut hooks) = B::hooks() else {
                        fstart_arch::halt();
                    };
                    if run_h3_bootblock::<B>(&mut hooks).is_err() {
                        fstart_arch::halt();
                    }
                    fstart_arch::halt()
                }
                StageEnvironment::Ram => run_h3_mainstage::<B>(handoff),
                StageEnvironment::Monolithic => fstart_arch::halt(),
            }
        }
    }

    /// Handwritten H3 SRAM bootblock sequence.
    pub fn run_h3_bootblock<B>(hooks: &mut B::Hooks) -> Result<(), ServiceError>
    where
        B: H3Board,
    {
        let config = B::CONFIG;
        let ccu = H3Ccu::new_from_config(&config.ccu);
        {
            let mut ctx = SunxiEarlyCtx::<H3>::new(&ccu, None);
            hooks.before_clock(&mut ctx)?;
        }
        ccu.init_early()?;
        {
            let mut ctx = SunxiEarlyCtx::<H3>::new(&ccu, None);
            hooks.after_clock(&mut ctx)?;
            hooks.before_console(&mut ctx)?;
        }

        let mut console =
            Ns16550::new(B::CONSOLE_CONFIG).map_err(|_| ServiceError::HardwareError)?;
        console.init().map_err(|_| ServiceError::HardwareError)?;
        // SAFETY: this bootblock never returns after installing its stack-owned console.
        unsafe { fstart_log::init(&console) };
        fstart_log::info!("h3: UART0 console ready");

        {
            let mut ctx = SunxiEarlyCtx::<H3>::new(&ccu, None);
            hooks.after_console(&mut ctx)?;
            hooks.before_memory(&mut ctx)?;
        }

        let mut dramc = H3Dramc::new_from_config(&config.dram);
        let dram_size = dramc.dram_init()?;
        {
            let mut ctx = SunxiEarlyCtx::<H3>::new(&ccu, Some(dram_size));
            hooks.after_memory(&mut ctx)?;
        }

        let mut mmc0 = H3Mmc::new_from_config(&config.mmc0);
        mmc0.init()?;
        if boot_device_at(H3_SRAM_BASE as usize) != BootDevice::Mmc0 {
            fstart_log::error!("h3: only eGON MMC0 boot is supported");
            return Err(ServiceError::NotSupported);
        }

        let next_stage_offset = u64::from(next_stage_offset_at(H3_SRAM_BASE as usize));
        let next_stage_size = next_stage_size_at(H3_SRAM_BASE as usize) as usize;
        validate_next_stage(config.mainstage_load_addr, next_stage_size, dram_size)?;
        let offset = H3_EGON_MMC_OFFSET
            .checked_add(next_stage_offset)
            .ok_or(ServiceError::InvalidParam)?;

        {
            let mut ctx = SunxiEarlyCtx::<H3>::new(&ccu, Some(dram_size));
            hooks.before_handoff(&mut ctx)?;
        }

        let read = fstart_stage::next_stage::read_stage_to_addr(
            &mmc0,
            "mmc0",
            "main",
            offset,
            config.mainstage_load_addr,
            next_stage_size,
        )?;
        if read != next_stage_size {
            return Err(ServiceError::IoError);
        }
        fstart_stage::next_stage::serialize_handoff(dram_size, config.handoff_addr)
            .map_err(|_| ServiceError::HardwareError)?;
        fstart_arch::armv7::jump_to_with_handoff(
            config.mainstage_load_addr,
            config.handoff_addr as usize,
        )
    }

    fn validate_next_stage(
        load_addr: u64,
        size: usize,
        dram_size: u64,
    ) -> Result<(), ServiceError> {
        if size == 0 || !in_h3_dram(load_addr) {
            return Err(ServiceError::InvalidParam);
        }
        let dram_end = H3_DRAM_BASE
            .checked_add(dram_size)
            .ok_or(ServiceError::InvalidParam)?;
        let end = load_addr
            .checked_add(size as u64)
            .ok_or(ServiceError::InvalidParam)?;
        if end > dram_end {
            return Err(ServiceError::InvalidParam);
        }
        Ok(())
    }

    /// Handwritten H3 DRAM mainstage setup.
    ///
    /// The generic FFS/payload launcher cannot use block-backed media yet;
    /// this retains the initialized devices until that launcher grows its
    /// driver-owned manifest buffer.
    pub fn run_h3_mainstage<B>(handoff: usize) -> !
    where
        B: H3Board,
    {
        let Some(handoff) = fstart_stage::handoff::try_deserialize(handoff) else {
            fstart_arch::halt();
        };
        let mut console = match Ns16550::new(B::CONSOLE_CONFIG) {
            Ok(console) => console,
            Err(_) => fstart_arch::halt(),
        };
        if console.init().is_err() {
            fstart_arch::halt();
        }
        // SAFETY: the mainstage owns this console until a later payload handoff.
        unsafe { fstart_log::init(&console) };
        fstart_log::info!("h3 mainstage: console ready");

        let mut mmc0 = H3Mmc::new_from_config(&B::CONFIG.mmc0);
        if mmc0.init().is_err() {
            fstart_log::error!("h3 mainstage: MMC0 init failed");
            fstart_arch::halt();
        }
        let mainstage = H3Mainstage {
            console,
            mmc0,
            dram_size: handoff.dram_size,
            config: B::CONFIG,
        };
        fstart_log::info!("h3 mainstage: {} MiB DRAM", mainstage.dram_size() >> 20);
        B::Payload::boot(mainstage)
    }
}

#[cfg(all(feature = "stage", feature = "h3", target_arch = "arm"))]
pub use stage::{
    return_to_fel, run_h3_bootblock, run_h3_mainstage, FelStash, H3Board, H3BuildSelectedPayload,
    H3Mainstage, SunxiEarlyBoard, SunxiEarlyBoardHooks, SunxiEarlyCtx, SunxiEarlyPlatform,
    SunxiPlatform, H3,
};

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
