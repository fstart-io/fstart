//! Handwritten QEMU SBSA-ref flow.

use crate::virt::QemuPciRootConfig;
use serde::Serialize;

pub const QEMU_SBSA_FLASH_BASE: u64 = 0x1000_0000;
pub const QEMU_SBSA_FLASH_SIZE: u64 = 0x1000_0000;
pub const QEMU_SBSA_RAM_BASE: u64 = 0x100_0000_0000;
pub const QEMU_SBSA_RAM_SIZE: u64 = 0x4000_0000;
pub const QEMU_SBSA_STAGE_LOAD_ADDR: u64 = 0x100_0010_0000;
pub const QEMU_SBSA_UART_BASE: u64 = 0x6000_0000;
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

#[cfg(all(feature = "bundle-sbsa", target_arch = "aarch64"))]
mod stage {
    use super::QemuSbsaConfig;
    use crate::virt::enumerate_pci;
    use fstart_core::services::ServiceError;
    use fstart_driver_uart::pl011::{Pl011, Pl011Config};
    use fstart_stage::{StageEnvironment, StageProgram, payload::MainstagePayload};

    #[cfg(not(all(fstart_stage_env = "monolithic", fstart_entry = "aarch64-relocate")))]
    compile_error!("SBSA requires monolithic/aarch64-relocate stage and entry selections");
    #[cfg(not(fstart_payload = "halt"))]
    compile_error!("SBSA selects the halt payload only");

    pub trait QemuSbsaBoard: 'static {
        type Payload: MainstagePayload<QemuSbsaMainstage>;
        const CONFIG: &'static QemuSbsaConfig;
    }

    /// Platform-owned adapter for fixed SBSA stage dispatch.
    pub struct QemuSbsaProgram<B>(core::marker::PhantomData<B>);
    impl<B: QemuSbsaBoard> StageProgram for QemuSbsaProgram<B> {
        fn run_stage(handoff: usize) -> ! {
            QemuSbsa::run_stage::<B>(StageEnvironment::Monolithic, handoff)
        }
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
            // TF-A hands BL33 x0 == 0 (no DTB crosses the secure boundary),
            // so RAM comes from the closed board config, not DTB discovery.
            let dtb_addr = fstart_arch::aarch64::boot_dtb_addr();
            let boot = if dtb_addr == 0 {
                crate::boot::from_static_ram(
                    B::CONFIG.ram_base,
                    B::CONFIG.ram_size,
                    super::QEMU_SBSA_FLASH_BASE + super::QEMU_SBSA_FFS_OFFSET,
                    super::QEMU_SBSA_FLASH_SIZE - super::QEMU_SBSA_FFS_OFFSET,
                )
            } else {
                crate::boot::from_dtb(
                    dtb_addr,
                    None,
                    B::CONFIG.ram_base,
                    super::QEMU_SBSA_FLASH_BASE + super::QEMU_SBSA_FFS_OFFSET,
                    super::QEMU_SBSA_FLASH_SIZE - super::QEMU_SBSA_FFS_OFFSET,
                )
            };
            if boot.is_err() {
                fstart_log::error!("qemu-sbsa: boot integrity setup failed");
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

#[cfg(all(feature = "bundle-sbsa", target_arch = "aarch64"))]
pub use stage::{QemuSbsa, QemuSbsaBoard, QemuSbsaMainstage, QemuSbsaProgram};
