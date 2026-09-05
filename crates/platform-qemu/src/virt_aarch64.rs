//! Handwritten QEMU AArch64 virt flow.

#[cfg(feature = "crabefi")]
use fstart_core::services::Console;
use fstart_core::services::ServiceError;
use fstart_driver_uart::pl011::{Pl011, Pl011Config};
use fstart_stage::payload::MainstagePayload;
use fstart_stage::{StageBoard, StageEnvironment};

use crate::virt::{QemuAarch64VirtConfig, enumerate_pci, phase};

pub const QEMU_AARCH64_UART_BASE: u64 = 0x0900_0000;

/// Board seams in the fixed AArch64 virt flow.
pub trait QemuAarch64VirtHooks {
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

pub trait QemuAarch64VirtBoard: StageBoard {
    type Hooks: QemuAarch64VirtHooks + Default;
    type Payload: MainstagePayload<QemuAarch64VirtMainstage>;

    const CONFIG: &'static QemuAarch64VirtConfig;
    fn console_config() -> Pl011Config;
}

pub struct QemuAarch64Virt;

pub struct QemuAarch64VirtMainstage {
    config: &'static QemuAarch64VirtConfig,
    console: Pl011,
    pci: Option<fstart_pci::PciEcam>,
}

impl QemuAarch64VirtMainstage {
    fn new<B: QemuAarch64VirtBoard>() -> Result<Self, ServiceError> {
        Ok(Self {
            config: B::CONFIG,
            console: Pl011::new(B::console_config())?,
            pci: None,
        })
    }

    fn init_console(&mut self) -> Result<(), ServiceError> {
        self.console.init()?;
        // SAFETY: the monolithic flow owns the console through payload handoff.
        unsafe { fstart_log::init(&self.console) };
        fstart_log::info!("qemu-aarch64: pl011 console ready");
        Ok(())
    }

    fn init_pci(&mut self) -> Result<(), ServiceError> {
        let pci = enumerate_pci(&self.config.pci)?;
        fstart_log::info!(
            "qemu-aarch64: PCI root ready ({} devices)",
            pci.device_count(),
        );
        self.pci = Some(pci);
        Ok(())
    }
}

/// DTB location for pflash/`-bios` boots.
///
/// QEMU only passes the DTB pointer in `x0` for direct `-kernel` boots; for
/// firmware boots it copies the DTB to the base of RAM instead. Fall back to
/// the configured RAM-base source when `x0` carried nothing.
fn source_dtb_addr(config: &super::virt::QemuAarch64VirtConfig) -> u64 {
    let boot = fstart_arch::aarch64::boot_dtb_addr();
    if boot != 0 {
        boot
    } else {
        config.source_dtb_addr
    }
}

#[cfg(feature = "linux")]
impl fstart_stage::payload::LinuxPayloadContext for QemuAarch64VirtMainstage {
    fn linux_payload_context(&self) -> fstart_stage::payload::LinuxPayloadConfig {
        let config = self.config.common;
        fstart_stage::payload::LinuxPayloadConfig::new(
            config.firmware_base,
            config.firmware_size,
            source_dtb_addr(&self.config),
            config.dtb_addr,
            config.kernel_addr,
            config.firmware_addr,
            config.ram_base,
            config.ram_size,
            0,
            config.bootargs,
        )
    }
}

#[cfg(feature = "crabefi")]
impl fstart_stage::payload::Aarch64UefiPayloadContext for QemuAarch64VirtMainstage {
    fn aarch64_uefi_payload_context(&self) -> fstart_stage::payload::Aarch64UefiPayloadConfig {
        let config = self.config.common;
        fstart_stage::payload::Aarch64UefiPayloadConfig::new(
            self.config.flash_base,
            self.config.flash_size,
            config.firmware_base,
            config.firmware_size,
            source_dtb_addr(&self.config),
            config.firmware_addr,
            config.ram_base,
            config.ram_size,
            self.config.pci.ecam_base,
        )
    }

    fn console(&self) -> &dyn Console {
        &self.console
    }
}

impl QemuAarch64Virt {
    pub fn run_stage<B>(env: StageEnvironment, handoff: usize) -> !
    where
        B: QemuAarch64VirtBoard,
    {
        let _ = handoff;
        if !matches!(env, StageEnvironment::Monolithic) {
            fstart_arch::halt();
        }
        let Ok(mut mainstage) = QemuAarch64VirtMainstage::new::<B>() else {
            fstart_arch::aarch64::halt();
        };
        let mut hooks = B::Hooks::default();
        if !phase("qemu-aarch64", "before_console", hooks.before_console())
            || !phase("qemu-aarch64", "console", mainstage.init_console())
            || !phase("qemu-aarch64", "after_console", hooks.after_console())
            || !phase(
                "qemu-aarch64",
                "boot_integrity",
                crate::boot::from_dtb(
                    source_dtb_addr(B::CONFIG),
                    cfg!(feature = "linux").then_some(B::CONFIG.common.dtb_addr),
                    B::CONFIG.common.ram_base,
                    B::CONFIG.common.firmware_base,
                    B::CONFIG.common.firmware_size,
                ),
            )
            || !phase("qemu-aarch64", "bus_scan", mainstage.init_pci())
            || !phase("qemu-aarch64", "before_payload", hooks.before_payload())
        {
            fstart_arch::aarch64::halt();
        }
        fstart_log::info!("qemu-aarch64 ramstage: finalize");
        fstart_log::info!("qemu-aarch64 ramstage: ready for payload");
        B::Payload::boot(mainstage)
    }
}
