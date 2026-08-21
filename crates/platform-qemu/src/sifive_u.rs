//! Direct QEMU `sifive_u` flow.
//!
//! QEMU enters the stage in live DRAM through `-bios`; unlike physical FU740
//! hardware this flow must not program PRCI or train DDR.

use serde::{Deserialize, Serialize};

#[cfg(feature = "host")]
use fstart_core::{
    BoardBuildPolicy, Compression, FdtSource, FirmwareConfig, FirmwareImageConfig,
    FirmwareImagePolicy, FirmwareKind, MemoryMap, MemoryRegion, MonolithicConfig, PayloadConfig,
    PayloadKind, QemuMachine, RegionKind, StageBuildConfig, StageLayout, hstr, hvec,
};

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

#[cfg(feature = "host")]
#[must_use]
pub fn qemu_sifive_u_memory() -> MemoryMap {
    let config = QemuSifiveUConfig::new();
    MemoryMap {
        regions: hvec([MemoryRegion {
            name: hstr("dram"),
            base: config.ram_base,
            size: config.ram_size,
            kind: RegionKind::Ram,
        }]),
        flash_layout: None,
        car: None,
    }
}

#[cfg(feature = "host")]
#[must_use]
pub fn qemu_sifive_u_stages() -> StageLayout {
    StageLayout::Monolithic(MonolithicConfig {
        build: StageBuildConfig {
            firmware_image: Some(FirmwareImageConfig {
                temp_ram_buffer: None,
            }),
            verify_firmware: true,
            payload: true,
            fdt: true,
            ..StageBuildConfig::default()
        },
        load_addr: QEMU_SIFIVE_U_FFS_BASE,
        stack_size: 0x40000,
        heap_size: Some(0x40000),
        data_addr: None,
        page_table_addr: None,
        page_size: Default::default(),
    })
}

#[cfg(feature = "host")]
#[must_use]
pub fn qemu_sifive_u_linux_payload() -> PayloadConfig {
    let config = QemuSifiveUConfig::new();
    PayloadConfig {
        kind: PayloadKind::LinuxBoot,
        kernel_file: Some(hstr("Image-riscv64")),
        kernel_load_addr: Some(config.kernel_addr),
        fdt: FdtSource::Platform,
        dtb_addr: Some(config.dtb_addr),
        src_dtb_addr: None,
        bootargs: Some(hstr(config.bootargs)),
        print_x86_mtrrs: false,
        compression: Compression::Lz4,
        firmware: Some(FirmwareConfig {
            kind: FirmwareKind::OpenSbi,
            file: hstr("fw_dynamic.bin"),
            load_addr: config.firmware_addr,
        }),
        fit_file: None,
        fit_config: None,
        fit_parse: None,
    }
}

#[cfg(feature = "host")]
#[must_use]
pub const fn qemu_sifive_u_build_policy() -> BoardBuildPolicy {
    BoardBuildPolicy {
        qemu_machine: Some(QemuMachine::SifiveU),
        firmware_image: FirmwareImagePolicy::memory_mapped(
            QEMU_SIFIVE_U_FFS_BASE,
            QEMU_SIFIVE_U_FFS_SIZE,
        ),
        flash_image: None,
        pci_root_feature: None,
        cpu_feature: None,
    }
}

#[cfg(all(feature = "stage", feature = "riscv64", target_arch = "riscv64"))]
mod stage {
    use super::*;
    #[cfg(feature = "crabefi")]
    use fstart_core::services::Console;
    use fstart_core::services::ServiceError;
    use fstart_driver_uart::sifive::{SifiveUart, SifiveUartConfig};
    use fstart_stage::{StageBoard, StageEnvironment, payload::MainstagePayload};

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

    /// Static QEMU `sifive_u` board contract.
    pub trait QemuSifiveUBoard: StageBoard {
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
            let _ = (env, handoff);
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
                0x3000_0000,
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
    QemuSifiveUMainstage,
};
