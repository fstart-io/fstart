//! QEMU platform flows and QEMU-only hardware blocks.

#![no_std]

#[cfg(feature = "stage")]
extern crate alloc;
extern crate ufmt;

pub mod fw_cfg;
pub mod q35;

#[cfg(feature = "stage")]
pub use stage::{run_qemu_q35_mainstage, QemuQ35, QemuQ35Board, QemuQ35Mainstage};

use fstart_core::{
    hstr, hvec, BoardBuildPolicy, FirmwareImageConfig, FirmwareImagePolicy, MemoryMap,
    MemoryRegion, MonolithicConfig, RegionKind, StageBuildConfig, StageLayout,
};
use serde::Serialize;

pub const QEMU_Q35_PLATFORM_NODE: &str = "q35";
pub const QEMU_Q35_FLASH_BASE: u64 = 0xff00_0000;
pub const QEMU_Q35_FLASH_SIZE: u64 = 0x0100_0000;
pub const QEMU_Q35_FFS_BASE: u64 = 0xff10_0000;
pub const QEMU_Q35_FFS_SIZE: u64 = 0x00ef_f000;
pub const QEMU_Q35_RAM_BASE: u64 = 0x0010_0000;
pub const QEMU_Q35_RAM_SIZE: u64 = 0x3ff0_0000;
pub const QEMU_Q35_DATA_ADDR: u64 = 0x0100_0000;
pub const QEMU_Q35_STACK_SIZE: u32 = 0x0040_0000;
pub const QEMU_Q35_HEAP_SIZE: u32 = 0x0020_0000;
pub const QEMU_Q35_PAGE_TABLE_ADDR: u64 = 0x1000;
pub const QEMU_Q35_PAGE_TABLE_SIZE: u64 = 0x4000;
pub const QEMU_Q35_ACPI_BUFFER_SIZE: usize = 512 * 1024;

pub const QEMU_Q35_UART_NODE: &str = "uart0";
pub const QEMU_Q35_UART_PIO_BASE: u64 = 0x3f8;
pub const QEMU_Q35_UART_CLOCK_FREQ: u32 = 1_843_200;
pub const QEMU_Q35_UART_BAUD_RATE: u32 = 115_200;

/// Closed QEMU q35 platform facts consumed by the handwritten QEMU flow.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QemuQ35Config {
    pub fw_cfg: fw_cfg::QemuFwCfgConfig,
    pub hostbridge: q35::Q35HostBridgeConfig,
    pub firmware_base: u64,
    pub firmware_size: u64,
}

impl QemuQ35Config {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            fw_cfg: fw_cfg::QemuFwCfgConfig::new(),
            hostbridge: q35::Q35HostBridgeConfig {
                ecam_base: 0xb000_0000,
                ecam_size: 0x1000_0000,
                bus_start: 0,
                bus_end: 255,
            },
            firmware_base: QEMU_Q35_FFS_BASE,
            firmware_size: QEMU_Q35_FFS_SIZE,
        }
    }

    #[must_use]
    pub const fn build(self) -> Self {
        if self.firmware_size == 0 {
            panic!("QEMU q35 firmware window must not be empty");
        }
        self
    }
}

impl Default for QemuQ35Config {
    fn default() -> Self {
        Self::new()
    }
}

#[must_use]
pub fn qemu_q35_memory() -> MemoryMap {
    MemoryMap {
        regions: hvec([
            MemoryRegion {
                name: hstr("flash"),
                base: QEMU_Q35_FLASH_BASE,
                size: QEMU_Q35_FLASH_SIZE,
                kind: RegionKind::Rom,
            },
            MemoryRegion {
                name: hstr("workram"),
                base: QEMU_Q35_RAM_BASE,
                size: QEMU_Q35_RAM_SIZE,
                kind: RegionKind::Ram,
            },
        ]),
        flash_layout: None,
        car: None,
    }
}

/// QEMU q35 uses one monolithic XIP stage: QEMU RAM is usable at reset, so a
/// bootblock/ramstage split would only add CI latency.
#[must_use]
pub fn qemu_q35_stages() -> StageLayout {
    StageLayout::Monolithic(MonolithicConfig {
        build: StageBuildConfig {
            firmware_image: Some(FirmwareImageConfig {
                temp_ram_buffer: None,
            }),
            verify_firmware: true,
            payload: true,
            pci: true,
            ..StageBuildConfig::default()
        },
        load_addr: QEMU_Q35_FLASH_BASE,
        stack_size: QEMU_Q35_STACK_SIZE,
        heap_size: Some(QEMU_Q35_HEAP_SIZE),
        data_addr: Some(QEMU_Q35_DATA_ADDR),
        page_table_addr: Some((QEMU_Q35_PAGE_TABLE_ADDR, QEMU_Q35_PAGE_TABLE_SIZE)),
        page_size: fstart_core::stage::PageSize::Size1GiB,
    })
}

#[must_use]
pub const fn qemu_q35_build_policy() -> BoardBuildPolicy {
    BoardBuildPolicy {
        firmware_image: FirmwareImagePolicy::memory_mapped(QEMU_Q35_FFS_BASE, QEMU_Q35_FFS_SIZE),
        pci_root_feature: None,
        cpu_feature: None,
    }
}

#[cfg(feature = "stage")]
mod stage {
    use fstart_core::services::memory_detect::{E820Entry, E820State};
    use fstart_core::services::{Console, ServiceError};
    use fstart_driver_uart::ns16550::{Ns16550, Ns16550Config};
    use fstart_stage::fixed_helpers::MemoryMappedFfs;
    use fstart_stage::payload::{MainstagePayload, X86UefiPayloadContext};
    use fstart_stage::{StageBoard, StageEnvironment};

    use crate::fw_cfg::QemuFwCfg;
    use crate::q35::Q35HostBridge;
    use crate::{QemuQ35Config, QEMU_Q35_ACPI_BUFFER_SIZE, QEMU_Q35_PLATFORM_NODE};

    pub struct QemuQ35;

    pub trait QemuQ35Board: StageBoard {
        type Payload: MainstagePayload<QemuQ35Mainstage>;

        const CONFIG: &'static QemuQ35Config;

        fn console_config() -> Ns16550Config;
        fn console_node() -> &'static str;
    }

    pub struct QemuQ35Mainstage {
        config: &'static QemuQ35Config,
        console: Ns16550,
        fw_cfg: QemuFwCfg,
        hostbridge: Q35HostBridge,
        e820: E820State,
        acpi_rsdp: Option<u64>,
    }

    impl QemuQ35Mainstage {
        fn new<B: QemuQ35Board>() -> Result<Self, ServiceError> {
            Ok(Self {
                config: B::CONFIG,
                console: Ns16550::new(B::console_config())
                    .map_err(|_| ServiceError::HardwareError)?,
                fw_cfg: QemuFwCfg::new(B::CONFIG.fw_cfg),
                hostbridge: Q35HostBridge::new(B::CONFIG.hostbridge)?,
                e820: E820State::new(),
                acpi_rsdp: None,
            })
        }

        fn firmware(&self) -> MemoryMappedFfs {
            MemoryMappedFfs::new(
                self.config.firmware_base,
                self.config.firmware_size as usize,
            )
        }

        fn init_console<B: QemuQ35Board>(&mut self) -> Result<(), ServiceError> {
            self.console
                .init()
                .map_err(|_| ServiceError::HardwareError)?;
            // SAFETY: the mainstage owns the console until payload handoff.
            unsafe { fstart_log::init(&self.console) };
            fstart_log::info!("{}: ns16550 console ready", B::console_node());
            fstart_log::info!("qemu-q35 ramstage (monolithic) console ready");
            Ok(())
        }

        fn detect_memory_and_tables(&mut self) -> Result<(), ServiceError> {
            self.fw_cfg.init()?;
            let count = self.fw_cfg.detect_memory(self.e820.entries_mut())?;
            let total = self.fw_cfg.total_ram_bytes()?;
            self.e820.set_detected(count, total);
            fstart_log::info!(
                "Detected {} MiB RAM, {} e820 entries from {}",
                total >> 20,
                count,
                QEMU_Q35_PLATFORM_NODE,
            );

            static mut ACPI_BUFFER: [u8; QEMU_Q35_ACPI_BUFFER_SIZE] =
                [0; QEMU_Q35_ACPI_BUFFER_SIZE];
            // SAFETY: firmware init is single-threaded; this buffer is written once.
            let acpi = unsafe { &mut *core::ptr::addr_of_mut!(ACPI_BUFFER) };
            self.acpi_rsdp = Some(self.fw_cfg.load_acpi_tables(acpi)?);
            Ok(())
        }

        fn init_pci(&mut self) -> Result<(), ServiceError> {
            self.hostbridge.init_with_e820(self.e820.entries())?;
            fstart_log::info!(
                "qemu-q35: PCI root ready ({} devices)",
                self.hostbridge.device_count(),
            );
            Ok(())
        }

        fn mount_boot_media(&self) -> Result<(), ServiceError> {
            fstart_arch::x86_64::enable_boot_media_rom_cache();
            self.firmware().mount()?;
            self.firmware().verify()
        }
    }

    impl X86UefiPayloadContext for QemuQ35Mainstage {
        fn console(&self) -> Option<&dyn Console> {
            Some(&self.console)
        }

        fn e820(&self) -> &[E820Entry] {
            self.e820.entries()
        }

        fn firmware_region(&self) -> (u64, u64) {
            (self.config.firmware_base, self.config.firmware_size)
        }

        fn acpi_rsdp(&self) -> Option<u64> {
            self.acpi_rsdp
        }

        fn ecam_base(&self) -> Option<u64> {
            Some(self.config.hostbridge.ecam_base)
        }
    }

    fn phase(name: &str, f: impl FnOnce() -> Result<(), ServiceError>) {
        fstart_log::info!("qemu-q35 ramstage: {}", name);
        if f().is_err() {
            fstart_log::error!("qemu-q35 ramstage: {} failed", name);
            fstart_arch::x86_64::halt();
        }
    }

    pub fn run_qemu_q35_mainstage<B>() -> !
    where
        B: QemuQ35Board,
    {
        let Ok(mut mainstage) = QemuQ35Mainstage::new::<B>() else {
            fstart_arch::x86_64::halt();
        };
        phase("console", || mainstage.init_console::<B>());
        phase("memory_detect", || mainstage.detect_memory_and_tables());
        phase("bus_scan", || mainstage.init_pci());
        phase("mount_boot_media", || mainstage.mount_boot_media());
        phase("finalize", || {
            fstart_log::info!("qemu-q35 ramstage: ready for payload");
            Ok(())
        });
        B::Payload::boot(mainstage)
    }

    impl QemuQ35 {
        pub fn run_stage<B>(env: StageEnvironment, _handoff: usize) -> !
        where
            B: QemuQ35Board,
        {
            let _ = env;
            run_qemu_q35_mainstage::<B>()
        }
    }
}
