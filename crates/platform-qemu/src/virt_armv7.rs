//! Handwritten QEMU ARMv7 virt flow.

use fstart_core::layout::{Layout, Region, RegionKind};
use fstart_core::services::ServiceError;
use fstart_driver_uart::pl011::{Pl011, Pl011Config};
use fstart_payload::MainstagePayload;

#[cfg(not(fstart_entry = "armv7"))]
compile_error!("ARMv7 virt requires the armv7 entry");
#[cfg(any(
    not(any(fstart_payload = "halt", fstart_payload = "linux")),
    all(fstart_payload = "halt", fstart_payload = "linux")
))]
compile_error!("select exactly one ARMv7 virt payload");
#[cfg(all(fstart_payload = "linux", not(feature = "linux")))]
compile_error!("selected Linux backend is not enabled");
#[cfg(fstart_payload = "halt")]
type Selected = fstart_payload::HaltPayload;
#[cfg(all(fstart_payload = "linux", feature = "linux"))]
type Selected = fstart_payload::LinuxPayload;

use crate::virt::{QemuArmv7VirtConfig, enumerate_pci, phase};

pub use crate::virt::QEMU_ARMV7_UART_BASE;

pub struct QemuArmv7VirtMainstage {
    config: &'static QemuArmv7VirtConfig,
    layout: Layout<'static>,
    console: Pl011,
    pci: Option<fstart_pci::PciEcam>,
}

impl QemuArmv7VirtMainstage {
    fn new(
        config: &'static QemuArmv7VirtConfig,
        console: Pl011Config,
    ) -> Result<Self, ServiceError> {
        let layout = fstart_stage::layout::current().map_err(|_| ServiceError::InvalidParam)?;
        for kind in [RegionKind::Firmware, RegionKind::Stack, RegionKind::Heap] {
            layout.region(kind).ok_or(ServiceError::InvalidParam)?;
        }
        #[cfg(fstart_payload = "linux")]
        for kind in [RegionKind::DeviceTree, RegionKind::Payload] {
            layout.region(kind).ok_or(ServiceError::InvalidParam)?;
        }
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

    fn init_console(&mut self) -> Result<(), ServiceError> {
        self.console.init()?;
        // SAFETY: the monolithic flow owns the console through payload handoff.
        unsafe { fstart_log::init(&self.console) };
        fstart_log::info!("qemu-armv7: pl011 console ready");
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
            "qemu-armv7: PCI root ready ({} devices)",
            pci.device_count(),
        );
        self.pci = Some(pci);
        Ok(())
    }
}

impl QemuArmv7VirtMainstage {
    /// Describe the machine to the build's payload launcher and boot it.
    fn hand_off(&self) -> ! {
        #[cfg(feature = "linux")]
        {
            let config = self.config;
            let ram_size = fstart_stage::directory::writable_memory()
                .and_then(|windows| windows.iter().find(|w| w.start == config.ram_base))
                .expect("ARMv7 virt RAM was not discovered")
                .size;
            let memory = crate::boot::ram_entry(config.ram_base, ram_size);
            let description =
                crate::boot::single_bank(&memory, &self.console, config.source_dtb_addr);
            let placement = fstart_payload::Placement {
                kernel: self.region(RegionKind::Payload).base,
                device_tree: self.region(RegionKind::DeviceTree).base,
                bootargs: config.bootargs,
                ..Default::default()
            };
            <Selected as MainstagePayload>::boot(
                fstart_payload::Handoff::new(description, placement),
                (),
            )
        }
        #[cfg(not(feature = "linux"))]
        <Selected as MainstagePayload>::boot(
            fstart_payload::Handoff::new(
                fstart_payload::Description::new(&[], Default::default()),
                Default::default(),
            ),
            (),
        )
    }
}

/// The QEMU Armv7 virt stage: console, boot media from the DTB, PCI, then
/// the payload. QEMU boards have no board work; their `main` calls this.
pub fn run(config: &'static QemuArmv7VirtConfig, console: Pl011Config) -> ! {
    fstart_stage::anchor::initial_stage();
    let Ok(mut mainstage) = QemuArmv7VirtMainstage::new(config, console) else {
        fstart_arch::halt();
    };
    if !phase("qemu-armv7", "console", mainstage.init_console())
        || !phase(
            "qemu-armv7",
            "boot_integrity",
            crate::boot::from_dtb_with_layout(
                config.source_dtb_addr,
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
        || !phase("qemu-armv7", "bus_scan", mainstage.init_pci())
    {
        fstart_arch::halt();
    }
    fstart_log::info!("qemu-armv7 ramstage: finalize");
    fstart_log::info!("qemu-armv7 ramstage: ready for payload");
    mainstage.hand_off()
}
