//! Handwritten FU740 clock, console, DRAM, and payload flow.

use crate::config::Fu740Config;

mod ddr;
mod prci;
mod regs;

/// FU740 DRAM physical base.
pub const FU740_DRAM_BASE: u64 = 0x8000_0000;
/// FU740 PCIe ECAM aperture used by its RISC-V UEFI payload context.
#[cfg(feature = "crabefi")]
pub const FU740_ECAM_BASE: u64 = 0x3000_0000;

mod stage {
    use super::*;
    #[cfg(feature = "crabefi")]
    use fstart_core::services::Console;
    use fstart_core::services::ServiceError;
    use super::{ddr::Fu740Ddr, prci::Fu740Prci};
    use fstart_driver_uart::sifive::{SifiveUart, SifiveUartConfig};
    use fstart_stage::{payload::MainstagePayload, StageBoard, StageEnvironment};

    /// Board seams in the fixed FU740 flow. Defaults compile away.
    pub trait Fu740Hooks {
        fn before_prci(&mut self) -> Result<(), ServiceError> {
            Ok(())
        }
        fn after_prci(&mut self) -> Result<(), ServiceError> {
            Ok(())
        }
        fn before_console(&mut self) -> Result<(), ServiceError> {
            Ok(())
        }
        fn after_console(&mut self) -> Result<(), ServiceError> {
            Ok(())
        }
        fn before_memory(&mut self) -> Result<(), ServiceError> {
            Ok(())
        }
        fn after_memory(&mut self) -> Result<(), ServiceError> {
            Ok(())
        }
        fn before_payload(&mut self) -> Result<(), ServiceError> {
            Ok(())
        }
    }

    /// Board contract for the FU740 flow.
    pub trait Fu740Board: StageBoard {
        type Hooks: Fu740Hooks + Default;
        type Payload: MainstagePayload<Fu740Mainstage>;

        /// ROM-resident platform policy; never copied onto the early stack.
        const CONFIG: &'static Fu740Config;
        /// Board-selected SiFive UART wiring and baud policy.
        const CONSOLE_CONFIG: &'static SifiveUartConfig;
    }

    /// DRAM-backed state retained through payload launch.
    pub struct Fu740Mainstage {
        config: &'static Fu740Config,
        console: SifiveUart,
    }

    impl Fu740Mainstage {
        #[must_use]
        pub const fn config(&self) -> &'static Fu740Config {
            self.config
        }

        #[must_use]
        pub const fn console(&self) -> &SifiveUart {
            &self.console
        }
    }

    /// Concrete FU740 fixed-flow platform.
    pub struct Fu740;

    impl Fu740 {
        /// Run PRCI, UART, DDR, then dispatch the build-selected FFS payload.
        pub fn run_stage<B>(env: StageEnvironment, handoff: usize) -> !
        where
            B: Fu740Board,
        {
            let _ = (env, handoff);
            let mut hooks = B::Hooks::default();
            if !phase("before_prci", hooks.before_prci()) {
                fstart_arch::riscv64::halt();
            }
            let mut prci = match Fu740Prci::new(&B::CONFIG.prci) {
                Ok(prci) => prci,
                Err(_) => fstart_arch::riscv64::halt(),
            };
            if !phase("prci", prci.init()) || !phase("after_prci", hooks.after_prci()) {
                fstart_arch::riscv64::halt();
            }

            if !phase("before_console", hooks.before_console()) {
                fstart_arch::riscv64::halt();
            }
            let mut console = match SifiveUart::new(B::CONSOLE_CONFIG) {
                Ok(console) => console,
                Err(_) => fstart_arch::riscv64::halt(),
            };
            if console.init().is_err() {
                fstart_arch::riscv64::halt();
            }
            // SAFETY: this stage owns the console through payload handoff.
            unsafe { fstart_log::init(&console) };
            fstart_log::info!("fu740: SiFive UART console ready");
            if !phase("after_console", hooks.after_console())
                || !phase("before_memory", hooks.before_memory())
            {
                fstart_arch::riscv64::halt();
            }

            let mut ddr = match Fu740Ddr::new(&B::CONFIG.ddr) {
                Ok(ddr) => ddr,
                Err(_) => fstart_arch::riscv64::halt(),
            };
            if !phase("ddr", ddr.init())
                || !phase("after_memory", hooks.after_memory())
                || !phase("before_payload", hooks.before_payload())
            {
                fstart_arch::riscv64::halt();
            }
            fstart_log::info!(
                "fu740 ramstage: {} MiB DRAM",
                ddr.detected_size_bytes() >> 20
            );
            fstart_log::info!("fu740 ramstage: ready for payload");
            B::Payload::boot(Fu740Mainstage {
                config: B::CONFIG,
                console,
            })
        }

        #[cfg(feature = "crabefi")]
        pub fn resume_sbi<B>(hart_id: u64, dtb_addr: u64) -> !
        where
            B: Fu740Board,
        {
            let mut console = match SifiveUart::new(B::CONSOLE_CONFIG) {
                Ok(console) => console,
                Err(_) => fstart_arch::riscv64::halt(),
            };
            if console.init().is_err() {
                fstart_arch::riscv64::halt();
            }
            // SAFETY: this resumed stage never returns; the console outlives CrabEFI.
            unsafe {
                fstart_log::replace_console(core::mem::transmute::<
                    &dyn Console,
                    &'static dyn Console,
                >(&console));
            }
            fstart_stage::payload::Riscv64UefiPayload::resume_sbi(
                Fu740Mainstage {
                    config: B::CONFIG,
                    console,
                },
                hart_id,
                dtb_addr,
            )
        }
    }

    fn phase(name: &str, result: Result<(), impl core::fmt::Debug>) -> bool {
        fstart_log::info!("fu740 ramstage: {}", name);
        if result.is_err() {
            fstart_log::error!("fu740 ramstage: {} failed", name);
            false
        } else {
            true
        }
    }

    #[cfg(feature = "linux")]
    impl fstart_stage::payload::LinuxPayloadContext for Fu740Mainstage {
        fn linux_payload_context(&self) -> fstart_stage::payload::LinuxPayloadConfig {
            let config = self.config;
            fstart_stage::payload::LinuxPayloadConfig::new(
                config.firmware_base,
                config.firmware_size,
                0,
                config.dtb_addr,
                config.kernel_addr,
                config.firmware_addr,
                FU740_DRAM_BASE,
                config.ddr.dram_size,
                config.boot_hart_id,
                config.bootargs,
            )
        }
    }

    #[cfg(all(feature = "crabefi", feature = "riscv64"))]
    impl fstart_stage::payload::Riscv64UefiPayloadContext for Fu740Mainstage {
        fn riscv64_uefi_payload_context(&self) -> fstart_stage::payload::Riscv64UefiPayloadConfig {
            let config = self.config;
            fstart_stage::payload::Riscv64UefiPayloadConfig::new(
                config.firmware_base,
                config.firmware_size,
                config.firmware_addr,
                0x20_0000,
                config.dtb_addr,
                FU740_DRAM_BASE,
                config.ddr.dram_size,
                FU740_ECAM_BASE,
            )
        }

        fn console(&self) -> &dyn Console {
            &self.console
        }
    }

    /// Linux payload launcher that requires the board-supplied FFS DTB.
    #[cfg(feature = "linux")]
    pub struct Fu740LinuxPayload;

    #[cfg(feature = "linux")]
    impl MainstagePayload<Fu740Mainstage> for Fu740LinuxPayload {
        fn boot(devices: Fu740Mainstage) -> ! {
            use fstart_stage::fixed_helpers::MemoryMappedLinuxBoot;

            let config = devices.config;
            let mut boot =
                MemoryMappedLinuxBoot::new(config.firmware_base, config.firmware_size as usize, 0);
            if boot.mount().is_err()
                || boot.verify().is_err()
                || boot.load_firmware_and_kernel().is_err()
                || boot.load_fdt(config.dtb_addr).is_err()
                || boot
                    .prepare_fdt(
                        config.dtb_addr,
                        config.bootargs,
                        FU740_DRAM_BASE,
                        config.ddr.dram_size,
                    )
                    .is_err()
            {
                fstart_log::error!("FU740 Linux payload: FFS payload load failed");
                fstart_arch::riscv64::halt();
            }
            fstart_arch::riscv64::boot_linux(&boot.boot_params(
                config.kernel_addr,
                config.firmware_addr,
                config.boot_hart_id,
                config.bootargs,
            ))
        }
    }

    #[cfg(feature = "linux")]
    pub type Fu740BuildSelectedPayload = Fu740LinuxPayload;
    #[cfg(all(not(feature = "linux"), feature = "crabefi"))]
    pub type Fu740BuildSelectedPayload = fstart_stage::payload::Riscv64UefiPayload;
    #[cfg(all(not(feature = "linux"), not(feature = "crabefi")))]
    pub type Fu740BuildSelectedPayload = fstart_stage::payload::HaltPayload;
}

#[cfg(all(feature = "stage", feature = "riscv64", target_arch = "riscv64"))]
pub use stage::{Fu740, Fu740Board, Fu740BuildSelectedPayload, Fu740Hooks};

