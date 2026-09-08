//! Handwritten QEMU ARMv7 virt flow.

use fstart_core::layout::{Layout, Region, RegionKind};
use fstart_core::services::ServiceError;
use fstart_driver_uart::pl011::{Pl011, Pl011Config};
use fstart_stage::payload::MainstagePayload;
use fstart_stage::{StageEnvironment, StageProgram};

#[cfg(not(all(fstart_stage_env = "monolithic", fstart_entry = "armv7")))]
compile_error!("ARMv7 virt requires monolithic/armv7 stage and entry selections");
#[cfg(any(
    not(any(fstart_payload = "halt", fstart_payload = "linux")),
    all(fstart_payload = "halt", fstart_payload = "linux")
))]
compile_error!("select exactly one ARMv7 virt payload");
#[cfg(all(fstart_payload = "linux", not(feature = "linux")))]
compile_error!("selected Linux backend is not enabled");
#[cfg(fstart_payload = "halt")]
type Selected = fstart_stage::payload::HaltPayload;
#[cfg(all(fstart_payload = "linux", feature = "linux"))]
type Selected = fstart_stage::payload::LinuxPayload;

use crate::virt::{QemuArmv7VirtConfig, enumerate_pci, phase};

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

pub trait QemuArmv7VirtBoard: 'static {
    type Hooks: QemuArmv7VirtHooks + Default;

    const CONFIG: &'static QemuArmv7VirtConfig;
    fn console_config() -> Pl011Config;
}

pub struct QemuArmv7Program<B>(core::marker::PhantomData<B>);
impl<B: QemuArmv7VirtBoard> StageProgram for QemuArmv7Program<B> {
    fn run_stage(handoff: usize) -> ! {
        QemuArmv7Virt::run_stage::<B>(StageEnvironment::Monolithic, handoff)
    }
}

pub struct QemuArmv7Virt;

pub struct QemuArmv7VirtMainstage {
    config: &'static QemuArmv7VirtConfig,
    layout: Layout<'static>,
    console: Pl011,
    pci: Option<fstart_pci::PciEcam>,
}

impl QemuArmv7VirtMainstage {
    fn new<B: QemuArmv7VirtBoard>() -> Result<Self, ServiceError> {
        let layout = fstart_stage::layout::current().map_err(|_| ServiceError::InvalidParam)?;
        for kind in [RegionKind::Firmware, RegionKind::Stack, RegionKind::Heap] {
            layout.region(kind).ok_or(ServiceError::InvalidParam)?;
        }
        #[cfg(fstart_payload = "linux")]
        for kind in [RegionKind::DeviceTree, RegionKind::Payload] {
            layout.region(kind).ok_or(ServiceError::InvalidParam)?;
        }
        Ok(Self {
            config: B::CONFIG,
            layout,
            console: Pl011::new(B::console_config())?,
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

#[cfg(feature = "linux")]
impl fstart_stage::payload::LinuxPayloadContext for QemuArmv7VirtMainstage {
    fn linux_payload_context(&self) -> fstart_stage::payload::LinuxPayloadConfig {
        let config = self.config;
        let firmware = crate::boot::mounted_image();
        let ram_size = fstart_stage::directory::writable_memory()
            .and_then(|windows| windows.iter().find(|w| w.start == config.ram_base))
            .expect("ARMv7 virt RAM was not discovered")
            .size;
        fstart_stage::payload::LinuxPayloadConfig::new(
            firmware.base,
            firmware.size,
            self.config.source_dtb_addr,
            self.region(RegionKind::DeviceTree).base,
            self.region(RegionKind::Payload).base,
            0,
            config.ram_base,
            ram_size,
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
        let _ = handoff;
        if !matches!(env, StageEnvironment::Monolithic) {
            fstart_arch::halt();
        }
        let Ok(mut mainstage) = QemuArmv7VirtMainstage::new::<B>() else {
            fstart_arch::halt();
        };
        let mut hooks = B::Hooks::default();
        if !phase("qemu-armv7", "before_console", hooks.before_console())
            || !phase("qemu-armv7", "console", mainstage.init_console())
            || !phase("qemu-armv7", "after_console", hooks.after_console())
            || !phase(
                "qemu-armv7",
                "boot_integrity",
                crate::boot::from_dtb_with_layout(
                    B::CONFIG.source_dtb_addr,
                    mainstage
                        .layout
                        .region(RegionKind::DeviceTree)
                        .map(|r| r.base),
                    B::CONFIG.ram_base,
                    mainstage.region(RegionKind::Firmware).base,
                    mainstage.region(RegionKind::Firmware).size,
                    Some(mainstage.layout),
                ),
            )
            || !phase("qemu-armv7", "bus_scan", mainstage.init_pci())
            || !phase("qemu-armv7", "before_payload", hooks.before_payload())
        {
            fstart_arch::halt();
        }
        fstart_log::info!("qemu-armv7 ramstage: finalize");
        fstart_log::info!("qemu-armv7 ramstage: ready for payload");
        <Selected as MainstagePayload<_>>::boot(mainstage)
    }
}
