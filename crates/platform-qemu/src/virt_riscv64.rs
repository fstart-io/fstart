//! Handwritten QEMU RISC-V virt flow with linked-descriptor stage geometry.

use crate::virt::{QemuRiscv64VirtConfig, enumerate_pci, phase};
use fstart_core::layout::{Layout, Region, RegionKind};
#[cfg(feature = "crabefi")]
use fstart_core::services::Console;
use fstart_core::services::ServiceError;
use fstart_driver_uart::ns16550::{Ns16550, Ns16550Config};
use fstart_stage::payload::MainstagePayload;

#[cfg(not(all(fstart_stage_env = "monolithic", fstart_entry = "riscv64")))]
compile_error!("RISC-V virt requires monolithic/riscv64 stage and entry selections");
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
compile_error!("select exactly one RISC-V virt payload");
#[cfg(all(fstart_payload = "linux", not(feature = "linux")))]
compile_error!("selected Linux backend is not enabled");
#[cfg(all(fstart_payload = "crabefi", not(feature = "crabefi")))]
compile_error!("selected CrabEFI backend is not enabled");

#[cfg(fstart_payload = "halt")]
type Selected = fstart_stage::payload::HaltPayload;
#[cfg(all(fstart_payload = "linux", feature = "linux"))]
type Selected = fstart_stage::payload::LinuxPayload;
#[cfg(all(fstart_payload = "crabefi", feature = "crabefi"))]
type Selected = fstart_stage::payload::Riscv64UefiPayload;

pub use crate::virt::QEMU_RISCV64_UART_BASE;

pub struct QemuRiscv64VirtMainstage {
    config: &'static QemuRiscv64VirtConfig,
    layout: Layout<'static>,
    console: Ns16550,
    pci: Option<fstart_pci::PciEcam>,
}

impl QemuRiscv64VirtMainstage {
    fn new(
        config: &'static QemuRiscv64VirtConfig,
        console: Ns16550Config,
    ) -> Result<Self, ServiceError> {
        let layout = fstart_stage::layout::current().map_err(|_| ServiceError::InvalidParam)?;
        for kind in [
            RegionKind::Image,
            RegionKind::Writable,
            RegionKind::Stack,
            RegionKind::Heap,
            RegionKind::Flash,
            RegionKind::Firmware,
        ] {
            layout.region(kind).ok_or(ServiceError::InvalidParam)?;
        }
        #[cfg(any(fstart_payload = "linux", fstart_payload = "crabefi"))]
        for kind in [RegionKind::DeviceTree, RegionKind::PayloadFirmware] {
            layout.region(kind).ok_or(ServiceError::InvalidParam)?;
        }
        #[cfg(fstart_payload = "linux")]
        layout
            .region(RegionKind::Payload)
            .ok_or(ServiceError::InvalidParam)?;
        Ok(Self {
            config,
            layout,
            console: Ns16550::new(console).map_err(|_| ServiceError::HardwareError)?,
            pci: None,
        })
    }

    #[cfg(any(feature = "linux", feature = "crabefi"))]
    fn ram_size(&self) -> u64 {
        // QEMU virt has one contiguous RAM bank. The retained platform load
        // policy holds its DTB-discovered size across the OpenSBI transition.
        fstart_stage::directory::writable_memory()
            .and_then(|windows| {
                windows
                    .iter()
                    .find(|window| window.start == self.config.ram_base)
            })
            .expect("RISC-V virt RAM was not discovered")
            .size
    }

    fn region(&self, kind: RegionKind) -> Region {
        self.layout
            .region(kind)
            .expect("resolved layout role missing")
    }

    fn init_console(&mut self) -> Result<(), ServiceError> {
        self.console
            .init()
            .map_err(|_| ServiceError::HardwareError)?;
        // SAFETY: the monolithic flow owns the console through payload handoff.
        unsafe { fstart_log::init(&self.console) };
        fstart_log::info!("qemu-riscv64: ns16550 console ready");
        fstart_log::info!(
            "resolved layout: stack={} heap={} firmware={}",
            self.region(RegionKind::Stack).size,
            self.region(RegionKind::Heap).size,
            self.region(RegionKind::Firmware).size
        );
        Ok(())
    }

    fn init_pci(&mut self) -> Result<(), ServiceError> {
        let pci = enumerate_pci(&self.config.pci)?;
        fstart_log::info!(
            "qemu-riscv64: PCI root ready ({} devices)",
            pci.device_count()
        );
        self.pci = Some(pci);
        Ok(())
    }
}

#[cfg(feature = "linux")]
impl fstart_stage::payload::LinuxPayloadContext for QemuRiscv64VirtMainstage {
    fn linux_payload_context(&self) -> fstart_stage::payload::LinuxPayloadConfig {
        let firmware = crate::boot::mounted_image();
        fstart_stage::payload::LinuxPayloadConfig::new(
            firmware.base,
            firmware.size,
            fstart_arch::riscv64::boot_dtb_addr(),
            self.region(RegionKind::DeviceTree).base,
            self.region(RegionKind::Payload).base,
            self.region(RegionKind::PayloadFirmware).base,
            self.config.ram_base,
            self.ram_size(),
            fstart_arch::riscv64::boot_hart_id(),
            self.config.bootargs,
        )
    }
}

#[cfg(feature = "crabefi")]
impl fstart_stage::payload::Riscv64UefiPayloadContext for QemuRiscv64VirtMainstage {
    fn riscv64_uefi_payload_context(&self) -> fstart_stage::payload::Riscv64UefiPayloadConfig {
        let firmware = crate::boot::mounted_image();
        let opensbi = self.region(RegionKind::PayloadFirmware);
        fstart_stage::payload::Riscv64UefiPayloadConfig::new(
            firmware.base,
            firmware.size,
            opensbi.base,
            opensbi.size,
            self.region(RegionKind::DeviceTree).base,
            self.config.ram_base,
            self.ram_size(),
            Some(fstart_pci::PciRootProvider::root_info(&self.config.pci)),
        )
    }
    fn console(&self) -> &dyn Console {
        &self.console
    }
}

/// Without CrabEFI nothing returns from OpenSBI here.
#[cfg(not(feature = "crabefi"))]
pub fn resume_sbi(_: &'static QemuRiscv64VirtConfig, _: Ns16550Config, _: u64, _: u64) -> ! {
    fstart_arch::riscv64::halt()
}

/// Recreate stage-local state after OpenSBI enters S-mode. The descriptor
/// remains in the same immutable, mapped flash image across this transition.
#[cfg(feature = "crabefi")]
pub fn resume_sbi(
    config: &'static QemuRiscv64VirtConfig,
    console: Ns16550Config,
    hart_id: u64,
    dtb_addr: u64,
) -> ! {
    let Ok(mut mainstage) = QemuRiscv64VirtMainstage::new(config, console) else {
        fstart_arch::riscv64::halt();
    };
    if mainstage.console.init().is_err() {
        fstart_arch::riscv64::halt();
    }
    // SAFETY: this resumed mainstage never returns, so its console outlives
    // CrabEFI. Replace the M-mode stack's stale logging backend.
    unsafe {
        fstart_log::replace_console(core::mem::transmute::<&dyn Console, &'static dyn Console>(
            &mainstage.console,
        ));
    }
    fstart_log::info!("riscv64 uefi: OpenSBI returned, launching CrabEFI");
    fstart_stage::payload::Riscv64UefiPayload::resume_sbi(mainstage, hart_id, dtb_addr)
}

/// The QEMU RISC-V virt stage: console, boot media from the DTB, PCI, then
/// the payload. QEMU boards have no board work; their `main` calls this.
pub fn run(config: &'static QemuRiscv64VirtConfig, console: Ns16550Config) -> ! {
    let Ok(mut mainstage) = QemuRiscv64VirtMainstage::new(config, console) else {
        fstart_arch::riscv64::halt();
    };
    let firmware = mainstage.region(RegionKind::Firmware);
    if !phase("qemu-riscv64", "console", mainstage.init_console())
        || !phase(
            "qemu-riscv64",
            "boot_integrity",
            crate::boot::from_dtb_with_layout(
                fstart_arch::riscv64::boot_dtb_addr(),
                mainstage
                    .layout
                    .region(RegionKind::DeviceTree)
                    .map(|r| r.base),
                config.ram_base,
                firmware.base,
                firmware.size,
                Some(mainstage.layout),
            ),
        )
        || !phase("qemu-riscv64", "bus_scan", mainstage.init_pci())
    {
        fstart_arch::riscv64::halt();
    }
    fstart_log::info!("qemu-riscv64 ramstage: finalize");
    fstart_log::info!("qemu-riscv64 ramstage: ready for payload");
    <Selected as MainstagePayload<_>>::boot(mainstage)
}
