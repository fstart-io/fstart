//! Handwritten QEMU AArch64 virt flow.

use fstart_core::layout::{Layout, Region, RegionKind};
#[cfg(feature = "crabefi")]
use fstart_core::services::Console;
use fstart_core::services::ServiceError;
use fstart_driver_uart::pl011::{Pl011, Pl011Config};
use fstart_stage::payload::MainstagePayload;

#[cfg(not(fstart_entry = "aarch64-relocate"))]
compile_error!("AArch64 virt requires the aarch64-relocate entry");
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
compile_error!("select exactly one AArch64 virt payload");
#[cfg(all(fstart_payload = "linux", not(feature = "linux")))]
compile_error!("selected Linux backend is not enabled");
#[cfg(all(fstart_payload = "crabefi", not(feature = "crabefi")))]
compile_error!("selected CrabEFI backend is not enabled");
#[cfg(fstart_payload = "halt")]
type Selected = fstart_stage::payload::HaltPayload;
#[cfg(all(fstart_payload = "linux", feature = "linux"))]
type Selected = fstart_stage::payload::LinuxPayload;
#[cfg(all(fstart_payload = "crabefi", feature = "crabefi"))]
type Selected = fstart_stage::payload::Aarch64UefiPayload;

use crate::virt::{QemuAarch64VirtConfig, enumerate_pci, phase};

pub use crate::virt::QEMU_AARCH64_UART_BASE;

pub struct QemuAarch64VirtMainstage {
    config: &'static QemuAarch64VirtConfig,
    layout: Layout<'static>,
    console: Pl011,
    pci: Option<fstart_pci::PciEcam>,
}

impl QemuAarch64VirtMainstage {
    fn new(
        config: &'static QemuAarch64VirtConfig,
        console: Pl011Config,
    ) -> Result<Self, ServiceError> {
        let layout = fstart_stage::layout::current().map_err(|_| ServiceError::InvalidParam)?;
        for kind in [
            RegionKind::Execution,
            RegionKind::Writable,
            RegionKind::Firmware,
            RegionKind::Flash,
            RegionKind::Stack,
            RegionKind::Heap,
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
            console: Pl011::new(console)?,
            pci: None,
        })
    }

    fn region(&self, kind: RegionKind) -> Region {
        self.layout
            .region(kind)
            .expect("resolved layout role missing")
    }

    #[cfg(any(feature = "linux", feature = "crabefi"))]
    fn ram_size(&self) -> u64 {
        fstart_stage::directory::writable_memory()
            .and_then(|windows| windows.iter().find(|w| w.start == self.config.ram_base))
            .expect("AArch64 virt RAM was not discovered")
            .size
    }

    fn prepare_device_tree(&self) -> Result<(), ServiceError> {
        #[cfg(fstart_payload = "crabefi")]
        fstart_stage::copy_fdt_to_workspace(
            source_dtb_addr(self.config),
            self.region(RegionKind::DeviceTree).base,
        )?;
        Ok(())
    }

    fn init_console(&mut self) -> Result<(), ServiceError> {
        self.console.init()?;
        // SAFETY: the monolithic flow owns the console through payload handoff.
        unsafe { fstart_log::init(&self.console) };
        fstart_log::info!("qemu-aarch64: pl011 console ready");
        fstart_log::info!(
            "resolved layout: execution={:#x} stack={} heap={} firmware={}",
            self.region(RegionKind::Execution).base,
            self.region(RegionKind::Stack).size,
            self.region(RegionKind::Heap).size,
            self.region(RegionKind::Firmware).size
        );
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
        let firmware = crate::boot::mounted_image();
        fstart_stage::payload::LinuxPayloadConfig::new(
            firmware.base,
            firmware.size,
            source_dtb_addr(self.config),
            self.region(RegionKind::DeviceTree).base,
            self.region(RegionKind::Payload).base,
            self.region(RegionKind::PayloadFirmware).base,
            self.config.ram_base,
            self.ram_size(),
            0,
            self.config.bootargs,
        )
    }
}

#[cfg(feature = "crabefi")]
impl fstart_stage::payload::Aarch64UefiPayloadContext for QemuAarch64VirtMainstage {
    fn aarch64_uefi_payload_context(&self) -> fstart_stage::payload::Aarch64UefiPayloadConfig {
        let flash = self.region(RegionKind::Flash);
        let firmware = crate::boot::mounted_image();
        fstart_stage::payload::Aarch64UefiPayloadConfig::new(
            flash.base,
            flash.size,
            firmware.base,
            firmware.size,
            self.region(RegionKind::DeviceTree).base,
            self.region(RegionKind::PayloadFirmware).base,
            self.config.ram_base,
            self.ram_size(),
            Some(fstart_pci::PciRootProvider::root_info(&self.config.pci)),
        )
    }

    fn console(&self) -> &dyn Console {
        &self.console
    }
}

/// The QEMU Aarch64 virt stage: console, boot media from the DTB, PCI, then
/// the payload. QEMU boards have no board work; their `main` calls this.
pub fn run(config: &'static QemuAarch64VirtConfig, console: Pl011Config) -> ! {
    fstart_stage::anchor::initial_stage();
    let Ok(mut mainstage) = QemuAarch64VirtMainstage::new(config, console) else {
        fstart_arch::aarch64::halt();
    };
    if !phase("qemu-aarch64", "console", mainstage.init_console())
        || !phase(
            "qemu-aarch64",
            "boot_integrity",
            crate::boot::from_dtb_with_layout(
                source_dtb_addr(config),
                mainstage
                    .layout
                    .region(RegionKind::DeviceTree)
                    .map(|r| r.base),
                config.ram_base,
                mainstage.region(RegionKind::Firmware).base,
                mainstage.region(RegionKind::Firmware).size,
                Some(mainstage.layout),
            ),
        )
        || !phase(
            "qemu-aarch64",
            "device_tree",
            mainstage.prepare_device_tree(),
        )
        || !phase("qemu-aarch64", "bus_scan", mainstage.init_pci())
    {
        fstart_arch::aarch64::halt();
    }
    fstart_log::info!("qemu-aarch64 ramstage: finalize");
    fstart_log::info!("qemu-aarch64 ramstage: ready for payload");
    <Selected as MainstagePayload<_>>::boot(mainstage)
}
