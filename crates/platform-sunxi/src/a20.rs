//! Allwinner A20 fixed SRAM and DRAM stage flow.

use fstart_core::handoff::HANDOFF_MAX_SIZE;
use fstart_driver_sunxi::a20_ccu::{A20CcuConfig, A20Uart};
use fstart_driver_sunxi::a20_dramc::A20DramcConfig;
use fstart_driver_sunxi::a20_mmc::{A20MmcConfig, A20MmcController};
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
        self
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
    use fstart_core::services::ServiceError;
    use fstart_driver_sunxi::a20_ccu::{A20_EGON_MMC_OFFSET, A20_SRAM_BASE, A20Ccu};
    use fstart_driver_sunxi::a20_dramc::A20Dramc;
    use fstart_driver_sunxi::a20_mmc::A20Mmc;
    use fstart_driver_uart::ns16550::{Ns16550, Ns16550Config};
    use fstart_stage::{StageBoard, StageEnvironment, payload::MainstagePayload};

    #[cfg(feature = "linux")]
    use crate::egon::ffs_total_size_at;
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

    /// Mutable context passed to the A20's board hooks.
    pub struct SunxiEarlyCtx<'a, P: SunxiEarlyPlatform> {
        ccu: &'a A20Ccu,
        dram_size: Option<u64>,
        _platform: core::marker::PhantomData<P>,
    }

    impl<'a, P: SunxiEarlyPlatform> SunxiEarlyCtx<'a, P> {
        fn new(ccu: &'a A20Ccu, dram_size: Option<u64>) -> Self {
            Self {
                ccu,
                dram_size,
                _platform: core::marker::PhantomData,
            }
        }

        #[must_use]
        pub const fn ccu(&self) -> &A20Ccu {
            self.ccu
        }

        #[must_use]
        pub const fn dram_size(&self) -> Option<u64> {
            self.dram_size
        }
    }

    /// Board-defined seams in the fixed A20 SRAM flow.
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

    /// Concrete Allwinner A20 platform.
    pub struct A20;

    impl SunxiPlatform for A20 {}
    impl SunxiEarlyPlatform for A20 {}

    /// Board facts required by the fixed A20 flow.
    pub trait A20Board: SunxiEarlyBoard<Platform = A20> {
        type Payload: fstart_stage::payload::MainstagePayload<A20Mainstage>;

        /// Board policy stored in `.rodata`; no pre-DRAM config transforms.
        const CONFIG: &'static A20Config;
        /// Board-selected console device policy, materialized in `.rodata`.
        const CONSOLE_CONFIG: Ns16550Config;
    }

    /// State retained by the DRAM-backed A20 mainstage.
    pub struct A20Mainstage {
        console: Ns16550,
        mmc0: A20Mmc,
        dram_size: u64,
        config: &'static super::A20Config,
    }

    impl A20Mainstage {
        #[must_use]
        pub const fn dram_size(&self) -> u64 {
            self.dram_size
        }

        #[must_use]
        pub const fn console(&self) -> &Ns16550 {
            &self.console
        }

        #[must_use]
        pub const fn mmc0(&self) -> &A20Mmc {
            &self.mmc0
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
    impl fstart_stage::payload::MainstagePayload<A20Mainstage> for A20LinuxPayload {
        fn boot(devices: A20Mainstage) -> ! {
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
            let ffs_size = ffs_total_size_at(A20_SRAM_BASE as usize) as usize;
            let mut boot =
                fstart_stage::fixed_helpers::BlockDeviceLinuxBoot::new(A20_EGON_MMC_OFFSET, 0);
            let anchor = fstart_stage::fstart_anchor_bytes();
            if boot.mount(&devices.mmc0, ffs_size, anchor).is_err()
                || boot.verify(&devices.mmc0).is_err()
                || boot.load_kernel(&devices.mmc0).is_err()
                || boot.load_fdt(&devices.mmc0).is_err()
            {
                fstart_log::error!("a20 mainstage: FFS payload load failed");
                fstart_arch::halt();
            }
            let config = devices.config;
            if config.fdt_dst_addr != 0
                && boot
                    .prepare_fdt(
                        config.fdt_dst_addr,
                        config.bootargs,
                        super::A20_DRAM_BASE,
                        devices.dram_size,
                    )
                    .is_err()
            {
                fstart_log::error!("a20 mainstage: FDT preparation failed");
                fstart_arch::halt();
            }
            let params = boot.boot_params(config.bootargs);
            fstart_arch::armv7::boot_linux(&params)
        }
    }

    /// Payload launcher selected by fbuild features for A20 boards.
    #[cfg(feature = "linux")]
    pub type A20BuildSelectedPayload = A20LinuxPayload;

    /// Payload launcher used when fbuild selected no payload backend.
    #[cfg(not(feature = "linux"))]
    pub type A20BuildSelectedPayload = fstart_stage::payload::HaltPayload;

    impl A20 {
        /// Dispatch the build-selected A20 SRAM or DRAM stage.
        pub fn run_stage<B>(env: StageEnvironment, handoff: usize) -> !
        where
            B: A20Board,
        {
            let _ = env;
            let _ = handoff;
            #[cfg(fstart_stage_env = "car")]
            {
                let Ok(mut hooks) = B::hooks() else {
                    fstart_arch::halt();
                };
                if run_a20_bootblock::<B>(&mut hooks).is_err() {
                    fstart_arch::halt();
                }
                fstart_arch::halt()
            }

            #[cfg(fstart_stage_env = "ram")]
            {
                run_a20_mainstage::<B>(handoff)
            }

            #[cfg(not(any(fstart_stage_env = "car", fstart_stage_env = "ram")))]
            match env {
                StageEnvironment::Car => {
                    let Ok(mut hooks) = B::hooks() else {
                        fstart_arch::halt();
                    };
                    if run_a20_bootblock::<B>(&mut hooks).is_err() {
                        fstart_arch::halt();
                    }
                    fstart_arch::halt()
                }
                StageEnvironment::Ram => run_a20_mainstage::<B>(handoff),
                StageEnvironment::Monolithic => fstart_arch::halt(),
            }
        }
    }

    /// Handwritten A20 SRAM bootblock sequence.
    pub fn run_a20_bootblock<B>(hooks: &mut B::Hooks) -> Result<(), ServiceError>
    where
        B: A20Board,
    {
        let config = B::CONFIG;
        let ccu = A20Ccu::new_from_config(&config.ccu);
        {
            let mut ctx = SunxiEarlyCtx::<A20>::new(&ccu, None);
            hooks.before_clock(&mut ctx)?;
        }
        ccu.init_early()?;
        {
            let mut ctx = SunxiEarlyCtx::<A20>::new(&ccu, None);
            hooks.after_clock(&mut ctx)?;
            hooks.before_console(&mut ctx)?;
        }

        let mut console =
            Ns16550::new(B::CONSOLE_CONFIG).map_err(|_| ServiceError::HardwareError)?;
        console.init().map_err(|_| ServiceError::HardwareError)?;
        // SAFETY: this bootblock never returns after installing its stack-owned console.
        unsafe { fstart_log::init(&console) };
        fstart_log::info!("a20: UART0 console ready");

        {
            let mut ctx = SunxiEarlyCtx::<A20>::new(&ccu, None);
            hooks.after_console(&mut ctx)?;
            hooks.before_memory(&mut ctx)?;
        }

        let mut dramc = A20Dramc::new_from_config(&config.dram);
        let dram_size = dramc.dram_init()?;
        {
            let mut ctx = SunxiEarlyCtx::<A20>::new(&ccu, Some(dram_size));
            hooks.after_memory(&mut ctx)?;
        }

        let mut mmc0 = A20Mmc::new_from_config(&config.mmc0);
        mmc0.init()?;
        if boot_device_at(A20_SRAM_BASE as usize) != BootDevice::Mmc0 {
            fstart_log::error!("a20: only eGON MMC0 boot is supported");
            return Err(ServiceError::NotSupported);
        }

        {
            let mut ctx = SunxiEarlyCtx::<A20>::new(&ccu, Some(dram_size));
            hooks.before_handoff(&mut ctx)?;
        }

        let entry = crate::boot::load_mainstage(
            &mmc0,
            A20_EGON_MMC_OFFSET,
            crate::egon::ffs_total_size_at(A20_SRAM_BASE as usize) as usize,
            A20_DRAM_BASE,
            dram_size,
            config.mainstage_load_addr,
            config.handoff_addr,
        )?;
        fstart_stage::next_stage::serialize_handoff(dram_size, config.handoff_addr)
            .map_err(|_| ServiceError::HardwareError)?;
        fstart_arch::armv7::jump_to_with_handoff(entry, config.handoff_addr as usize)
    }

    /// Handwritten A20 DRAM mainstage setup.
    ///
    /// The generic FFS/payload launcher cannot use block-backed media yet;
    /// this retains the initialized devices until that launcher grows its
    /// driver-owned manifest buffer.
    pub fn run_a20_mainstage<B>(handoff: usize) -> !
    where
        B: A20Board,
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

        let mut mmc0 = A20Mmc::new_from_config(&B::CONFIG.mmc0);
        if mmc0.init().is_err() {
            fstart_log::error!("a20 mainstage: MMC0 init failed");
            fstart_arch::halt();
        }
        let mainstage = A20Mainstage {
            console,
            mmc0,
            dram_size: handoff.dram_size,
            config: B::CONFIG,
        };
        fstart_log::info!("a20 mainstage: {} MiB DRAM", mainstage.dram_size() >> 20);
        B::Payload::boot(mainstage)
    }
}

#[cfg(all(feature = "stage", feature = "a20", target_arch = "arm"))]
pub use stage::{
    A20, A20Board, A20BuildSelectedPayload, A20Mainstage, FelStash, SunxiEarlyBoard,
    SunxiEarlyBoardHooks, SunxiEarlyCtx, SunxiEarlyPlatform, SunxiPlatform, return_to_fel,
    run_a20_bootblock, run_a20_mainstage,
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
