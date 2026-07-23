//! Handwritten QEMU RISC-V virt flow.

#[cfg(feature = "crabefi")]
use fstart_core::services::Console;
use fstart_core::services::ServiceError;
use fstart_driver_uart::ns16550::{Ns16550, Ns16550Config};
use fstart_stage::payload::MainstagePayload;
use fstart_stage::{StageBoard, StageEnvironment};

use crate::virt::{enumerate_pci, phase, QemuRiscv64VirtConfig};
#[cfg(feature = "crabefi")]
use crate::virt::{QEMU_RISCV64_OPENSBI_RESERVE_SIZE, QEMU_RISCV64_UEFI_DTB_ADDR};

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
    config: &'static QemuRiscv64VirtConfig,
    console: Ns16550,
    pci: Option<fstart_pci::PciEcam>,
}

impl QemuRiscv64VirtMainstage {
    fn new<B: QemuRiscv64VirtBoard>() -> Result<Self, ServiceError> {
        Ok(Self {
            config: B::CONFIG,
            console: Ns16550::new(B::console_config()).map_err(|_| ServiceError::HardwareError)?,
            pci: None,
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

    fn init_pci(&mut self) -> Result<(), ServiceError> {
        let pci = enumerate_pci(&self.config.pci)?;
        fstart_log::info!(
            "qemu-riscv64: PCI root ready ({} devices)",
            pci.device_count(),
        );
        self.pci = Some(pci);
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

#[cfg(feature = "crabefi")]
impl fstart_stage::payload::Riscv64UefiPayloadContext for QemuRiscv64VirtMainstage {
    fn riscv64_uefi_payload_context(&self) -> fstart_stage::payload::Riscv64UefiPayloadConfig {
        let config = self.config.common;
        fstart_stage::payload::Riscv64UefiPayloadConfig::new(
            config.firmware_base,
            config.firmware_size,
            config.firmware_addr,
            QEMU_RISCV64_OPENSBI_RESERVE_SIZE,
            QEMU_RISCV64_UEFI_DTB_ADDR,
            config.ram_base,
            config.ram_size,
            self.config.pci.ecam_base,
        )
    }

    fn console(&self) -> &dyn Console {
        &self.console
    }
}

impl QemuRiscv64Virt {
    /// Recreate the minimal mainstage state after OpenSBI enters S-mode.
    #[cfg(feature = "crabefi")]
    pub fn resume_sbi<B>(hart_id: u64, dtb_addr: u64) -> !
    where
        B: QemuRiscv64VirtBoard,
    {
        let Ok(mut mainstage) = QemuRiscv64VirtMainstage::new::<B>() else {
            fstart_arch::riscv64::halt();
        };
        if mainstage.console.init().is_err() {
            fstart_arch::riscv64::halt();
        }
        // SAFETY: this resumed mainstage never returns, so its console outlives
        // CrabEFI. Replace the M-mode stack's stale logging backend.
        unsafe {
            fstart_log::replace_console(
                core::mem::transmute::<&dyn Console, &'static dyn Console>(&mainstage.console),
            );
        }
        fstart_log::info!("riscv64 uefi: OpenSBI returned, launching CrabEFI");
        fstart_stage::payload::Riscv64UefiPayload::resume_sbi(mainstage, hart_id, dtb_addr)
    }

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
            || !phase("qemu-riscv64", "bus_scan", mainstage.init_pci())
            || !phase("qemu-riscv64", "before_payload", hooks.before_payload())
        {
            fstart_arch::riscv64::halt();
        }
        fstart_log::info!("qemu-riscv64 ramstage: finalize");
        fstart_log::info!("qemu-riscv64 ramstage: ready for payload");
        B::Payload::boot(mainstage)
    }
}
