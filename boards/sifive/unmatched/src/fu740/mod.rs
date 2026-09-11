//! Handwritten FU740 clock, console, DRAM, and payload flow.

use crate::config::Fu740Config;

mod ddr;
mod prci;
mod regs;

/// FU740 DRAM physical base.
pub const FU740_DRAM_BASE: u64 = 0x8000_0000;
/// FU740 PCIe ECAM aperture used by its RISC-V UEFI payload context.
#[cfg(fstart_payload = "crabefi")]
pub const FU740_ECAM_BASE: u64 = 0x3000_0000;

mod stage {
    use super::*;
    use super::{ddr::Fu740Ddr, prci::Fu740Prci};
    #[cfg(fstart_payload = "crabefi")]
    use fstart_core::services::Console;
    use fstart_core::services::ServiceError;
    use fstart_driver_uart::sifive::{SifiveUart, SifiveUartConfig};
    // The board selects payload backends through platform features; all
    // stage runtime paths below resolve through that re-export.
    use fstart_platform_qemu::stage_runtime as fstart_stage;
    use fstart_stage::{StageEnvironment, StageProgram, payload::MainstagePayload};

    #[cfg(not(all(
        fstart_stage_env = "monolithic",
        fstart_entry = "riscv64"
    )))]
    compile_error!("FU740 requires monolithic/riscv64 stage and entry selections");
    #[cfg(any(
        not(any(
            fstart_payload = "halt",
            fstart_payload = "linux",
            fstart_payload = "crabefi"
        )),
        all(fstart_payload = "halt", fstart_payload = "linux"),
        all(fstart_payload = "halt", fstart_payload = "crabefi"),
        all(fstart_payload = "linux", fstart_payload = "crabefi")
    ))]
    compile_error!("select exactly one FU740 payload");

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

    /// Platform-owned adapter for fixed FU740 stage dispatch.
    pub struct Fu740Program<B>(core::marker::PhantomData<B>);
    impl<B: Fu740Board> StageProgram for Fu740Program<B> {
        fn run_stage(handoff: usize) -> ! {
            Fu740::run_stage::<B>(StageEnvironment::Monolithic, handoff)
        }
        #[cfg(fstart_payload = "crabefi")]
        fn resume_sbi(hart_id: u64, dtb_addr: u64) -> ! {
            Fu740::resume_sbi::<B>(hart_id, dtb_addr)
        }
    }

    /// Board contract for the FU740 flow.
    pub trait Fu740Board: 'static {
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
            if !matches!(env, StageEnvironment::Monolithic) {
                fstart_arch::riscv64::halt();
            }
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
                || !phase(
                    "boot_integrity",
                    install_boot_context(B::CONFIG, ddr.detected_size_bytes(), handoff),
                )
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

        #[cfg(fstart_payload = "crabefi")]
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

    /// This monolithic flow authenticates its own root, not an inherited one.
    /// The board has no established protected initial verifier or rollback store.
    fn install_boot_context(
        config: &Fu740Config,
        detected_size: u64,
        handoff: usize,
    ) -> Result<(), ServiceError> {
        {
            use fstart_stage::boot::{MemoryPolicy, MemoryWindow};
            use fstart_stage::fixed_helpers::MemoryMappedFfs;

            // Never grant more RAM than the trusted DDR profile initialized,
            // even if the controller filter reports a larger address window.
            let size = detected_size.min(config.ddr.dram_size);
            if size == 0 || FU740_DRAM_BASE.checked_add(size).is_none() {
                return Err(ServiceError::InvalidParam);
            }
            let writable = [MemoryWindow {
                start: FU740_DRAM_BASE,
                size,
            }];
            let stage = fstart_stage::boot::running_stage_windows()
                .map_err(|_| ServiceError::InvalidParam)?;
            let workspace = MemoryWindow {
                start: config.dtb_addr,
                size: 64 * 1024,
            };
            let source = {
                #[cfg(all(fstart_payload = "crabefi", not(fstart_payload = "linux")))]
                {
                    let readable = [writable[0]];
                    boot_dtb_window(workspace, &readable)?
                }
                #[cfg(not(all(fstart_payload = "crabefi", not(fstart_payload = "linux"))))]
                {
                    workspace
                }
            };
            let reserved = [
                stage[0],
                stage[1],
                MemoryWindow {
                    start: config.firmware_base,
                    size: config.firmware_size,
                },
                if source == workspace {
                    MemoryWindow { start: 0, size: 0 }
                } else {
                    source
                },
                // No inherited directory is imported. Still keep any supplied
                // handoff bytes out of all loader destinations for this boot.
                MemoryWindow {
                    start: handoff as u64,
                    size: if handoff == 0 {
                        0
                    } else {
                        fstart_core::handoff::HANDOFF_MAX_SIZE as u64
                    },
                },
            ];
            if source != workspace {
                let source_reserved = [reserved[0], reserved[1], reserved[2], reserved[4]];
                let source_policy = MemoryPolicy {
                    writable: &writable,
                    reserved: &source_reserved,
                    entry_alignment: 4,
                };
                if !source_policy.permits(source.start, source.size) {
                    return Err(ServiceError::InvalidParam);
                }
            }
            let policy = MemoryPolicy {
                writable: &writable,
                reserved: &reserved,
                entry_alignment: 4,
            };
            // SAFETY: only initialized DRAM is writable. Linker ranges cover
            // the full live image, heap and stack in LIM; SPI and handoff are
            // excluded. This fixed flow starts no DMA devices or secondary
            // harts and configures no external temporary allocation arena.
            unsafe { fstart_stage::directory::set_load_policy(&policy) }
                .map_err(|_| ServiceError::InvalidParam)?;
            #[cfg(any(fstart_payload = "linux", fstart_payload = "crabefi"))]
            // SAFETY: source is either a bounded boot DTB in trusted readable
            // memory, or the dedicated destination for an authenticated FFS DTB.
            // Registration checks destination RAM and all live reservations.
            unsafe { fstart_stage::configure_fdt_workspace(source, workspace) }?;
            fstart_log::info!(
                "fu740: development integrity; no hardware secure boot or rollback enforcement"
            );
            let firmware = MemoryMappedFfs::new(
                config.firmware_base,
                usize::try_from(config.firmware_size).map_err(|_| ServiceError::InvalidParam)?,
            );
            firmware.mount()?;
            firmware.verify()
        }
    }

    #[cfg(all(fstart_payload = "crabefi", not(fstart_payload = "linux")))]
    fn boot_dtb_window(
        destination: fstart_stage::boot::MemoryWindow,
        readable: &[fstart_stage::boot::MemoryWindow],
    ) -> Result<fstart_stage::boot::MemoryWindow, ServiceError> {
        use fstart_stage::boot::{MemoryPolicy, MemoryWindow};
        let address = fstart_arch::riscv64::boot_dtb_addr();
        if address == 0 {
            return Ok(destination);
        }
        let bounds = MemoryPolicy {
            writable: readable,
            reserved: &[],
            entry_alignment: 4,
        };
        if address & 3 != 0 || !bounds.permits(address, 8) {
            return Err(ServiceError::InvalidParam);
        }
        // SAFETY: header lies in the controller/profile-bounded DRAM window.
        let size = u64::from(u32::from_be(unsafe {
            core::ptr::read_unaligned((address + 4) as *const u32)
        }));
        if !(40..=destination.size).contains(&size) || !bounds.permits(address, size) {
            return Err(ServiceError::InvalidParam);
        }
        Ok(MemoryWindow {
            start: address,
            size,
        })
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

    #[cfg(fstart_payload = "linux")]
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

    #[cfg(all(fstart_payload = "crabefi", target_arch = "riscv64"))]
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
    #[cfg(fstart_payload = "linux")]
    pub struct Fu740LinuxPayload;

    #[cfg(fstart_payload = "linux")]
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

    #[cfg(fstart_payload = "linux")]
    pub type Fu740BuildSelectedPayload = Fu740LinuxPayload;
    #[cfg(all(not(fstart_payload = "linux"), fstart_payload = "crabefi"))]
    pub type Fu740BuildSelectedPayload = fstart_stage::payload::Riscv64UefiPayload;
    #[cfg(all(not(fstart_payload = "linux"), not(fstart_payload = "crabefi")))]
    pub type Fu740BuildSelectedPayload = fstart_stage::payload::HaltPayload;
}

#[cfg(all(fstart_stage_env = "monolithic", target_arch = "riscv64"))]
pub use stage::{Fu740, Fu740Board, Fu740BuildSelectedPayload, Fu740Hooks, Fu740Program};
