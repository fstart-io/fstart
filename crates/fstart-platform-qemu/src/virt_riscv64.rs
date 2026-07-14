//! Handwritten QEMU RISC-V virt flow.

use fstart_core::services::ServiceError;
use fstart_driver_uart::ns16550::{Ns16550, Ns16550Config};
use fstart_stage::payload::MainstagePayload;
use fstart_stage::{StageBoard, StageEnvironment};

use crate::virt::{phase, QemuRiscv64VirtConfig};

pub const QEMU_RISCV64_UART_BASE: u64 = 0x1000_0000;

/// Board seams in the fixed RISC-V virt flow.
pub trait QemuRiscv64VirtHooks {
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

/// Static RISC-V virt board contract. Config is ROM data, hooks are board code.
pub trait QemuRiscv64VirtBoard: StageBoard {
    type Hooks: QemuRiscv64VirtHooks + Default;
    type Payload: MainstagePayload<QemuRiscv64VirtMainstage>;

    const CONFIG: &'static QemuRiscv64VirtConfig;
    fn console_config() -> Ns16550Config;
}

pub struct QemuRiscv64Virt;

pub struct QemuRiscv64VirtMainstage {
    #[cfg(feature = "linux")]
    config: &'static QemuRiscv64VirtConfig,
    console: Ns16550,
}

impl QemuRiscv64VirtMainstage {
    fn new<B: QemuRiscv64VirtBoard>() -> Result<Self, ServiceError> {
        Ok(Self {
            #[cfg(feature = "linux")]
            config: B::CONFIG,
            console: Ns16550::new(B::console_config()).map_err(|_| ServiceError::HardwareError)?,
        })
    }

    fn init_console(&mut self) -> Result<(), ServiceError> {
        self.console
            .init()
            .map_err(|_| ServiceError::HardwareError)?;
        // SAFETY: the monolithic flow owns the console through payload handoff.
        unsafe { fstart_log::init(&self.console) };
        fstart_log::info!("qemu-riscv64: ns16550 console ready");
        Ok(())
    }
}

#[cfg(feature = "linux")]
impl fstart_stage::payload::LinuxPayloadContext for QemuRiscv64VirtMainstage {
    fn linux_payload_context(&self) -> fstart_stage::payload::LinuxPayloadConfig {
        let config = self.config.common;
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

impl QemuRiscv64Virt {
    pub fn run_stage<B>(env: StageEnvironment, handoff: usize) -> !
    where
        B: QemuRiscv64VirtBoard,
    {
        let _ = (env, handoff);
        let Ok(mut mainstage) = QemuRiscv64VirtMainstage::new::<B>() else {
            fstart_arch::riscv64::halt();
        };
        let mut hooks = B::Hooks::default();
        if !phase("qemu-riscv64", "before_console", hooks.before_console())
            || !phase("qemu-riscv64", "console", mainstage.init_console())
            || !phase("qemu-riscv64", "after_console", hooks.after_console())
            || !phase("qemu-riscv64", "before_payload", hooks.before_payload())
        {
            fstart_arch::riscv64::halt();
        }
        fstart_log::info!("qemu-riscv64 ramstage: finalize");
        fstart_log::info!("qemu-riscv64 ramstage: ready for payload");
        B::Payload::boot(mainstage)
    }
}
