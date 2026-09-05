//! Handwritten QEMU SBSA-ref flow.

use crate::virt::QemuPciRootConfig;
#[cfg(feature = "host")]
use fstart_core::{
    BoardBuildPolicy, FirmwareImageConfig, FirmwareImagePolicy, MemoryMap, MemoryRegion,
    MonolithicConfig, RegionKind, StageBuildConfig, StageLayout, hstr, hvec,
};
use serde::Serialize;

pub const QEMU_SBSA_FLASH_BASE: u64 = 0x1000_0000;
pub const QEMU_SBSA_FLASH_SIZE: u64 = 0x1000_0000;
pub const QEMU_SBSA_RAM_BASE: u64 = 0x100_0000_0000;
pub const QEMU_SBSA_RAM_SIZE: u64 = 0x4000_0000;
pub const QEMU_SBSA_STAGE_LOAD_ADDR: u64 = 0x100_0010_0000;
pub const QEMU_SBSA_UART_BASE: u64 = 0x6000_0000;
#[cfg(any(
    feature = "host",
    all(feature = "stage", feature = "aarch64", target_arch = "aarch64")
))]
const QEMU_SBSA_FFS_OFFSET: u64 = 0x10_0000;

/// Closed SBSA-ref facts consumed by the fixed flow.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QemuSbsaConfig {
    pub ram_base: u64,
    pub ram_size: u64,
    pub uart_base: u64,
    pub pci: QemuPciRootConfig,
}

impl QemuSbsaConfig {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            ram_base: QEMU_SBSA_RAM_BASE,
            ram_size: QEMU_SBSA_RAM_SIZE,
            uart_base: QEMU_SBSA_UART_BASE,
            pci: QemuPciRootConfig::new(
                0xf000_0000,
                0xff,
                0x8000_0000,
                0x7000_0000,
                0x0000_0001_0000_0000,
                0x0000_00ff_0000_0000,
            ),
        }
    }

    #[must_use]
    pub const fn build(self) -> Self {
        if self.ram_size == 0 {
            panic!("SBSA RAM must not be empty");
        }
        let _ = self.pci.build();
        self
    }
}

impl Default for QemuSbsaConfig {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "host")]
#[must_use]
pub fn qemu_sbsa_memory() -> MemoryMap {
    MemoryMap {
        regions: hvec([
            MemoryRegion {
                name: hstr("flash"),
                base: QEMU_SBSA_FLASH_BASE,
                size: QEMU_SBSA_FLASH_SIZE,
                kind: RegionKind::Rom,
            },
            MemoryRegion {
                name: hstr("ram"),
                base: QEMU_SBSA_RAM_BASE,
                size: QEMU_SBSA_RAM_SIZE,
                kind: RegionKind::Ram,
            },
        ]),
        flash_layout: None,
        car: None,
    }
}

#[cfg(feature = "host")]
#[must_use]
pub fn qemu_sbsa_stages() -> StageLayout {
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
        load_addr: QEMU_SBSA_STAGE_LOAD_ADDR,
        stack_size: 0x40_000,
        heap_size: Some(0x40_000),
        data_addr: Some(QEMU_SBSA_STAGE_LOAD_ADDR + 0x20_0000),
        page_table_addr: None,
        page_size: Default::default(),
    })
}

#[cfg(feature = "host")]
#[must_use]
pub fn qemu_sbsa_build_policy() -> BoardBuildPolicy {
    BoardBuildPolicy {
        qemu_machine: Some(fstart_core::QemuMachine::SbsaRef),
        // Leave flash offset zero for the TF-A-loaded BL33 reset stub.
        firmware_image: FirmwareImagePolicy::memory_mapped(
            QEMU_SBSA_FLASH_BASE + QEMU_SBSA_FFS_OFFSET,
            QEMU_SBSA_FLASH_SIZE - QEMU_SBSA_FFS_OFFSET,
        ),
        flash_image: Some(FirmwareImagePolicy::memory_mapped(
            QEMU_SBSA_FLASH_BASE,
            QEMU_SBSA_FLASH_SIZE,
        )),
        pci_root_feature: None,
        cpu_feature: None,
    }
}

#[cfg(all(feature = "stage", feature = "aarch64", target_arch = "aarch64"))]
mod stage {
    use super::QemuSbsaConfig;
    use crate::virt::enumerate_pci;
    use fstart_core::services::ServiceError;
    use fstart_driver_uart::pl011::{Pl011, Pl011Config};
    use fstart_stage::{StageBoard, StageEnvironment, payload::MainstagePayload};

    pub trait QemuSbsaBoard: StageBoard {
        type Payload: MainstagePayload<QemuSbsaMainstage>;
        const CONFIG: &'static QemuSbsaConfig;
    }

    pub struct QemuSbsa;
    pub struct QemuSbsaMainstage {
        config: &'static QemuSbsaConfig,
        console: Pl011,
        pci: Option<fstart_pci::PciEcam>,
    }

    impl QemuSbsaMainstage {
        fn new<B: QemuSbsaBoard>() -> Result<Self, ServiceError> {
            Ok(Self {
                config: B::CONFIG,
                console: Pl011::new(Pl011Config {
                    base_addr: B::CONFIG.uart_base,
                    clock_freq: 1_843_200,
                    baud_rate: 115_200,
                })?,
                pci: None,
            })
        }
    }

    impl QemuSbsaMainstage {
        fn init_pci(&mut self) -> Result<(), ServiceError> {
            let pci = enumerate_pci(&self.config.pci)?;
            fstart_log::info!("qemu-sbsa: PCI root ready ({} devices)", pci.device_count(),);
            self.pci = Some(pci);
            Ok(())
        }
    }

    impl QemuSbsa {
        pub fn run_stage<B: QemuSbsaBoard>(env: StageEnvironment, handoff: usize) -> ! {
            let _ = handoff;
            if !matches!(env, StageEnvironment::Monolithic) {
                fstart_arch::halt();
            }
            let Ok(mut mainstage) = QemuSbsaMainstage::new::<B>() else {
                fstart_arch::aarch64::halt()
            };
            if mainstage.console.init().is_err() {
                fstart_arch::aarch64::halt()
            }
            unsafe { fstart_log::init(&mainstage.console) };
            fstart_log::info!("qemu-sbsa: pl011 console ready");
            if crate::boot::from_dtb(
                fstart_arch::aarch64::boot_dtb_addr(),
                None,
                B::CONFIG.ram_base,
                super::QEMU_SBSA_FLASH_BASE + super::QEMU_SBSA_FFS_OFFSET,
                super::QEMU_SBSA_FLASH_SIZE - super::QEMU_SBSA_FFS_OFFSET,
            )
            .is_err()
            {
                fstart_log::error!(
                    "qemu-sbsa: boot integrity requires a supported RAM discovery DTB"
                );
                fstart_arch::halt();
            }
            if mainstage.init_pci().is_err() {
                fstart_log::error!("qemu-sbsa ramstage: bus_scan failed");
                fstart_arch::aarch64::halt()
            }
            fstart_log::info!("qemu-sbsa ramstage: ready for payload");
            B::Payload::boot(mainstage)
        }
    }
}

#[cfg(all(feature = "stage", feature = "aarch64", target_arch = "aarch64"))]
pub use stage::{QemuSbsa, QemuSbsaBoard, QemuSbsaMainstage};
