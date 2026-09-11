//! Direct QEMU `sifive_u` flow.
//!
//! QEMU enters the stage in live DRAM through `-bios`; unlike physical FU740
//! hardware this flow must not program PRCI or train DDR.

use serde::{Deserialize, Serialize};

pub const QEMU_SIFIVE_U_FFS_BASE: u64 = 0x8000_0000;
pub const QEMU_SIFIVE_U_FFS_SIZE: u64 = 0x1000_0000;
pub const QEMU_SIFIVE_U_RAM_SIZE: u64 = 0x4000_0000;
pub const QEMU_SIFIVE_U_DTB_ADDR: u64 = 0x8f00_0000;
pub const QEMU_SIFIVE_U_KERNEL_ADDR: u64 = 0x8400_0000;
pub const QEMU_SIFIVE_U_FIRMWARE_ADDR: u64 = 0x8300_0000;
pub const QEMU_SIFIVE_U_BOOTARGS: &str = "console=ttySIF0 earlycon=sbi";

/// Closed QEMU `sifive_u` policy consumed by its direct flow.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QemuSifiveUConfig {
    pub firmware_base: u64,
    pub firmware_size: u64,
    pub ram_base: u64,
    pub ram_size: u64,
    pub dtb_addr: u64,
    pub kernel_addr: u64,
    pub firmware_addr: u64,
    pub bootargs: &'static str,
}

impl QemuSifiveUConfig {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            firmware_base: QEMU_SIFIVE_U_FFS_BASE,
            firmware_size: QEMU_SIFIVE_U_FFS_SIZE,
            ram_base: QEMU_SIFIVE_U_FFS_BASE,
            ram_size: QEMU_SIFIVE_U_RAM_SIZE,
            dtb_addr: QEMU_SIFIVE_U_DTB_ADDR,
            kernel_addr: QEMU_SIFIVE_U_KERNEL_ADDR,
            firmware_addr: QEMU_SIFIVE_U_FIRMWARE_ADDR,
            bootargs: QEMU_SIFIVE_U_BOOTARGS,
        }
    }

    #[must_use]
    pub const fn build(self) -> Self {
        if self.firmware_size == 0
            || self.ram_size == 0
            || self.firmware_base.checked_add(self.firmware_size).is_none()
            || self.ram_base.checked_add(self.ram_size).is_none()
        {
            panic!("QEMU sifive_u firmware and RAM windows must be valid");
        }
        self
    }
}

impl Default for QemuSifiveUConfig {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(all(feature = "stage", feature = "riscv64", target_arch = "riscv64"))]
mod stage {
    use super::*;
    #[cfg(feature = "crabefi")]
    use fstart_core::services::Console;
    use fstart_core::services::ServiceError;
    use fstart_driver_uart::sifive::{SifiveUart, SifiveUartConfig};
    use fstart_stage::{StageEnvironment, StageProgram, payload::MainstagePayload};

    #[cfg(not(all(fstart_stage_env = "monolithic", fstart_entry = "riscv64")))]
    compile_error!("sifive-u requires monolithic/riscv64 stage and entry selections");
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
    compile_error!("select exactly one sifive-u payload");
    #[cfg(all(fstart_payload = "linux", not(feature = "linux")))]
    compile_error!("selected Linux backend is not enabled");
    #[cfg(all(fstart_payload = "crabefi", not(feature = "crabefi")))]
    compile_error!("selected CrabEFI backend is not enabled");

    /// Board seams in the direct QEMU `sifive_u` flow.
    pub trait QemuSifiveUHooks {
        fn before_console(&mut self) -> Result<(), ServiceError> {
            Ok(())
        }
        fn after_console(&mut self) -> Result<(), ServiceError> {
            Ok(())
        }
        fn before_payload(&mut self) -> Result<(), ServiceError> {
            Ok(())
        }
    }

    /// Platform-owned adapter for fixed sifive-u stage dispatch.
    pub struct QemuSifiveUProgram<B>(core::marker::PhantomData<B>);
    impl<B: QemuSifiveUBoard> StageProgram for QemuSifiveUProgram<B> {
        fn run_stage(handoff: usize) -> ! {
            QemuSifiveU::run_stage::<B>(StageEnvironment::Monolithic, handoff)
        }
        #[cfg(feature = "crabefi")]
        fn resume_sbi(hart_id: u64, dtb_addr: u64) -> ! {
            QemuSifiveU::resume_sbi::<B>(hart_id, dtb_addr)
        }
    }

    /// Static QEMU `sifive_u` board contract.
    pub trait QemuSifiveUBoard: 'static {
        type Hooks: QemuSifiveUHooks + Default;
        type Payload: MainstagePayload<QemuSifiveUMainstage>;

        const CONFIG: &'static QemuSifiveUConfig;
        const CONSOLE_CONFIG: &'static SifiveUartConfig;
    }

    /// DRAM-resident state retained through selected payload launch.
    pub struct QemuSifiveUMainstage {
        config: &'static QemuSifiveUConfig,
        console: SifiveUart,
    }

    impl QemuSifiveUMainstage {
        #[must_use]
        pub const fn console(&self) -> &SifiveUart {
            &self.console
        }
    }

    /// QEMU `sifive_u` fixed flow marker.
    pub struct QemuSifiveU;

    impl QemuSifiveU {
        pub fn run_stage<B>(env: StageEnvironment, handoff: usize) -> !
        where
            B: QemuSifiveUBoard,
        {
            let _ = handoff;
            if !matches!(env, StageEnvironment::Monolithic) {
                fstart_arch::halt();
            }
            let mut hooks = B::Hooks::default();
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
            // SAFETY: the stage owns this console through payload handoff.
            unsafe { fstart_log::init(&console) };
            fstart_log::info!("sifive-u: SiFive UART console ready");
            if !phase("after_console", hooks.after_console())
                || !phase(
                    "boot_integrity",
                    crate::boot::from_dtb(
                        fstart_arch::riscv64::boot_dtb_addr(),
                        cfg!(any(feature = "linux", feature = "crabefi"))
                            .then_some(B::CONFIG.dtb_addr),
                        B::CONFIG.ram_base,
                        B::CONFIG.firmware_base,
                        B::CONFIG.firmware_size,
                    ),
                )
                || !phase("before_payload", hooks.before_payload())
            {
                fstart_arch::riscv64::halt();
            }
            fstart_log::info!("sifive-u ramstage: ready for payload");
            B::Payload::boot(QemuSifiveUMainstage {
                config: B::CONFIG,
                console,
            })
        }

        #[cfg(feature = "crabefi")]
        pub fn resume_sbi<B>(hart_id: u64, dtb_addr: u64) -> !
        where
            B: QemuSifiveUBoard,
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
                QemuSifiveUMainstage {
                    config: B::CONFIG,
                    console,
                },
                hart_id,
                dtb_addr,
            )
        }
    }

    fn phase(name: &str, result: Result<(), ServiceError>) -> bool {
        fstart_log::info!("sifive-u ramstage: {}", name);
        if result.is_err() {
            fstart_log::error!("sifive-u ramstage: {} failed", name);
            false
        } else {
            true
        }
    }

    #[cfg(feature = "linux")]
    impl fstart_stage::payload::LinuxPayloadContext for QemuSifiveUMainstage {
        fn linux_payload_context(&self) -> fstart_stage::payload::LinuxPayloadConfig {
            let config = self.config;
            fstart_stage::payload::LinuxPayloadConfig::new(
                config.firmware_base,
                config.firmware_size,
                fstart_arch::riscv64::boot_dtb_addr(),
                config.dtb_addr,
                config.kernel_addr,
                config.firmware_addr,
                config.ram_base,
                config.ram_size,
                fstart_arch::riscv64::boot_hart_id(),
                config.bootargs,
            )
        }
    }

    #[cfg(all(feature = "crabefi", feature = "riscv64"))]
    impl fstart_stage::payload::Riscv64UefiPayloadContext for QemuSifiveUMainstage {
        fn riscv64_uefi_payload_context(&self) -> fstart_stage::payload::Riscv64UefiPayloadConfig {
            let config = self.config;
            fstart_stage::payload::Riscv64UefiPayloadConfig::new(
                config.firmware_base,
                config.firmware_size,
                config.firmware_addr,
                0x20_0000,
                config.dtb_addr,
                config.ram_base,
                config.ram_size,
                // No described PCI root: let CrabEFI inspect the firmware FDT,
                // rather than inventing bus bounds from a bare ECAM address.
                None,
            )
        }

        fn console(&self) -> &dyn Console {
            &self.console
        }
    }

    #[cfg(feature = "linux")]
    pub type QemuSifiveUBuildSelectedPayload = fstart_stage::payload::LinuxPayload;
    #[cfg(all(not(feature = "linux"), feature = "crabefi"))]
    pub type QemuSifiveUBuildSelectedPayload = fstart_stage::payload::Riscv64UefiPayload;
    #[cfg(all(not(feature = "linux"), not(feature = "crabefi")))]
    pub type QemuSifiveUBuildSelectedPayload = fstart_stage::payload::HaltPayload;
}

#[cfg(all(feature = "stage", feature = "riscv64", target_arch = "riscv64"))]
pub use stage::{
    QemuSifiveU, QemuSifiveUBoard, QemuSifiveUBuildSelectedPayload, QemuSifiveUHooks,
    QemuSifiveUMainstage, QemuSifiveUProgram,
};
