//! Handwritten QEMU ARMv7 virt flow.

use fstart_core::services::ServiceError;
use fstart_driver_uart::pl011::{Pl011, Pl011Config};
use fstart_stage::payload::MainstagePayload;
use fstart_stage::{StageBoard, StageEnvironment};

use crate::virt::{enumerate_pci, phase, QemuArmv7VirtConfig};

pub const QEMU_ARMV7_UART_BASE: u64 = 0x0900_0000;

/// Board seams in the fixed ARMv7 virt flow.
pub trait QemuArmv7VirtHooks {
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

pub trait QemuArmv7VirtBoard: StageBoard {
    type Hooks: QemuArmv7VirtHooks + Default;
    type Payload: MainstagePayload<QemuArmv7VirtMainstage>;

    const CONFIG: &'static QemuArmv7VirtConfig;
    fn console_config() -> Pl011Config;
}

pub struct QemuArmv7Virt;

pub struct QemuArmv7VirtMainstage {
    config: &'static QemuArmv7VirtConfig,
    console: Pl011,
    pci: Option<fstart_pci::PciEcam>,
}

impl QemuArmv7VirtMainstage {
    fn new<B: QemuArmv7VirtBoard>() -> Result<Self, ServiceError> {
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
        fstart_log::info!("qemu-armv7: pl011 console ready");
        Ok(())
    }

    fn init_pci(&mut self) -> Result<(), ServiceError> {
        let pci = enumerate_pci(&self.config.pci)?;
        fstart_log::info!(
            "qemu-armv7: PCI root ready ({} devices)",
            pci.device_count(),
        );
        self.pci = Some(pci);
        Ok(())
    }
}

#[cfg(feature = "linux")]
impl fstart_stage::payload::LinuxPayloadContext for QemuArmv7VirtMainstage {
    fn linux_payload_context(&self) -> fstart_stage::payload::LinuxPayloadConfig {
        let config = self.config.common;
        fstart_stage::payload::LinuxPayloadConfig::new(
            config.firmware_base,
            config.firmware_size,
            self.config.source_dtb_addr,
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

impl QemuArmv7Virt {
    pub fn run_stage<B>(env: StageEnvironment, handoff: usize) -> !
    where
        B: QemuArmv7VirtBoard,
    {
        let _ = (env, handoff);
        let Ok(mut mainstage) = QemuArmv7VirtMainstage::new::<B>() else {
            fstart_arch::halt();
        };
        let mut hooks = B::Hooks::default();
        if !phase("qemu-armv7", "before_console", hooks.before_console())
            || !phase("qemu-armv7", "console", mainstage.init_console())
            || !phase("qemu-armv7", "after_console", hooks.after_console())
            || !phase("qemu-armv7", "bus_scan", mainstage.init_pci())
            || !phase("qemu-armv7", "before_payload", hooks.before_payload())
        {
            fstart_arch::halt();
        }
        fstart_log::info!("qemu-armv7 ramstage: finalize");
        fstart_log::info!("qemu-armv7 ramstage: ready for payload");
        B::Payload::boot(mainstage)
    }
}
