//! QEMU platform flows and QEMU-only hardware blocks.

#![no_std]

#[cfg(any(feature = "host", feature = "stage"))]
extern crate alloc;
extern crate ufmt;

#[cfg(feature = "stage")]
mod boot;
#[cfg(any(test, feature = "stage"))]
mod dtb_memory;

/// QEMU bochs-display init. Platform-owned like coreboot's
/// `drivers/emulation/qemu/bochs.c`: it programs this platform's exact
/// display device and advertises the mode via the common [`FramebufferInfo`]
/// handoff, it is not a generic display driver.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub mod display;

pub mod facts;
#[cfg(feature = "host")]
pub mod host;
#[cfg(feature = "host")]
pub use host::Plan;

#[cfg(feature = "stage")]
#[doc(hidden)]
pub use fstart_stage as stage_runtime;
#[macro_export]
macro_rules! stage_bin {
    ($program:ty) => { $crate::stage_runtime::stage_bin!(program: $program); };
}

pub mod fw_cfg;
pub mod sbsa;
pub mod sifive_u;
pub mod virt;

pub use sbsa::QemuSbsaConfig;
pub use sifive_u::QemuSifiveUConfig;
pub use virt::qemu_virt_security_config;
pub use virt::{
    QemuAarch64VirtConfig, QemuArmv7VirtConfig, QemuPciRootConfig, QemuRiscv64VirtConfig,
};
#[cfg(all(feature = "stage", feature = "virt-aarch64", target_arch = "aarch64"))]
pub mod virt_aarch64;
#[cfg(all(feature = "stage", feature = "armv7", target_arch = "arm"))]
pub mod virt_armv7;
#[cfg(all(feature = "stage", feature = "virt-riscv64", target_arch = "riscv64"))]
pub mod virt_riscv64;

#[cfg(all(feature = "bundle-sbsa", target_arch = "aarch64"))]
pub use sbsa::{QemuSbsa, QemuSbsaBoard, QemuSbsaMainstage, QemuSbsaProgram};
#[cfg(all(feature = "stage", feature = "riscv64", target_arch = "riscv64"))]
pub use sifive_u::{
    QemuSifiveU, QemuSifiveUBoard, QemuSifiveUBuildSelectedPayload, QemuSifiveUHooks,
    QemuSifiveUMainstage, QemuSifiveUProgram,
};
#[cfg(all(feature = "stage", feature = "virt-aarch64", target_arch = "aarch64"))]
pub use virt_aarch64::{
    QemuAarch64Program, QemuAarch64Virt, QemuAarch64VirtBoard, QemuAarch64VirtHooks,
    QemuAarch64VirtMainstage,
};
#[cfg(all(feature = "stage", feature = "armv7", target_arch = "arm"))]
pub use virt_armv7::{
    QemuArmv7Program, QemuArmv7Virt, QemuArmv7VirtBoard, QemuArmv7VirtHooks, QemuArmv7VirtMainstage,
};
#[cfg(all(feature = "stage", feature = "virt-riscv64", target_arch = "riscv64"))]
pub use virt_riscv64::{
    QemuRiscv64Program, QemuRiscv64Virt, QemuRiscv64VirtBoard, QemuRiscv64VirtHooks,
    QemuRiscv64VirtMainstage,
};
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub mod q35;
#[cfg(all(feature = "smm", any(target_arch = "x86", target_arch = "x86_64")))]
mod q35_smm;
/// SMM image ABI and handler binding for the q35 SMM flow.
///
/// Mirrors `fstart-platform-intel`'s `bundle-smm` re-export: board `smm.rs`
/// binds its handler through `fstart_platform_qemu::smm::smm_bin!`.
#[cfg(feature = "smm")]
pub use fstart_smm as smm;

#[cfg(all(
    feature = "bundle-q35",
    any(target_arch = "x86", target_arch = "x86_64")
))]
pub use stage::{QemuQ35, QemuQ35Board, QemuQ35Mainstage, QemuQ35Program, run_qemu_q35_mainstage};

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
use serde::Serialize;

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub const QEMU_Q35_PLATFORM_NODE: &str = "q35";
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub const QEMU_Q35_FLASH_BASE: u64 = 0xff00_0000;
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub const QEMU_Q35_FLASH_SIZE: u64 = 0x0100_0000;
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub const QEMU_Q35_FFS_BASE: u64 = 0xff10_0000;
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub const QEMU_Q35_FFS_SIZE: u64 = 0x00ef_f000;
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub const QEMU_Q35_RAM_BASE: u64 = 0x0010_0000;
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub const QEMU_Q35_RAM_SIZE: u64 = 0x3ff0_0000;
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub const QEMU_Q35_DATA_ADDR: u64 = 0x0100_0000;
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub const QEMU_Q35_STACK_SIZE: u32 = 0x0040_0000;
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub const QEMU_Q35_HEAP_SIZE: u32 = 0x0020_0000;
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub const QEMU_Q35_PAGE_TABLE_ADDR: u64 = 0x1000;
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub const QEMU_Q35_PAGE_TABLE_SIZE: u64 = 0x4000;
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub const QEMU_Q35_ACPI_BUFFER_SIZE: usize = 512 * 1024;

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub const QEMU_Q35_UART_NODE: &str = "uart0";
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub const QEMU_Q35_UART_PIO_BASE: u64 = 0x3f8;
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub const QEMU_Q35_UART_CLOCK_FREQ: u32 = 1_843_200;
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub const QEMU_Q35_UART_BAUD_RATE: u32 = 115_200;

/// Closed QEMU q35 platform facts consumed by the handwritten QEMU flow.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QemuQ35Config {
    pub fw_cfg: fw_cfg::QemuFwCfgConfig,
    pub hostbridge: q35::Q35HostBridgeConfig,
    pub firmware_base: u64,
    pub firmware_size: u64,
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
impl QemuQ35Config {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            fw_cfg: fw_cfg::QemuFwCfgConfig::new(),
            hostbridge: q35::Q35HostBridgeConfig {
                ecam_base: 0xb000_0000,
                ecam_size: 0x1000_0000,
                bus_start: 0,
                bus_end: 255,
            },
            firmware_base: QEMU_Q35_FFS_BASE,
            firmware_size: QEMU_Q35_FFS_SIZE,
        }
    }

    #[must_use]
    pub const fn build(self) -> Self {
        if self.firmware_size == 0 {
            panic!("QEMU q35 firmware window must not be empty");
        }
        self
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
impl Default for QemuQ35Config {
    fn default() -> Self {
        Self::new()
    }
}

/// QEMU q35 uses one monolithic XIP stage: QEMU RAM is usable at reset, so a
/// bootblock/ramstage split would only add CI latency.
#[cfg(all(
    feature = "bundle-q35",
    any(target_arch = "x86", target_arch = "x86_64")
))]
mod stage {
    use crate::display::{BochsDisplay, BochsDisplayConfig};
    use fstart_core::services::memory_detect::{E820Entry, E820State};
    use fstart_core::services::{Console, ServiceError};
    use fstart_driver_uart::ns16550::{Ns16550, Ns16550Config};
    use fstart_stage::payload::{MainstagePayload, X86UefiPayloadContext};
    use fstart_stage::{StageEnvironment, StageProgram};

    use crate::fw_cfg::QemuFwCfg;
    use crate::q35::Q35HostBridge;
    use crate::{QEMU_Q35_ACPI_BUFFER_SIZE, QEMU_Q35_PLATFORM_NODE, QemuQ35Config};

    #[cfg(not(all(fstart_stage_env = "monolithic", fstart_entry = "x86_64")))]
    compile_error!("QEMU q35 requires monolithic/x86_64 stage and entry selections");
    #[cfg(any(
        not(any(fstart_payload = "halt", fstart_payload = "crabefi")),
        all(fstart_payload = "halt", fstart_payload = "crabefi")
    ))]
    compile_error!("select exactly one q35 payload");
    #[cfg(all(fstart_payload = "crabefi", not(feature = "crabefi")))]
    compile_error!("selected CrabEFI backend is not enabled");

    pub struct QemuQ35;

    /// Platform-owned adapter for fixed q35 stage dispatch.
    pub struct QemuQ35Program<B>(core::marker::PhantomData<B>);
    impl<B: QemuQ35Board> StageProgram for QemuQ35Program<B> {
        fn run_stage(handoff: usize) -> ! {
            QemuQ35::run_stage::<B>(StageEnvironment::Monolithic, handoff)
        }
    }

    pub trait QemuQ35Board: 'static {
        type Payload: MainstagePayload<QemuQ35Mainstage>;

        const CONFIG: &'static QemuQ35Config;

        fn console_config() -> Ns16550Config;
        fn console_node() -> &'static str;
    }

    /// Native SMM handler image built by fbuild, embedded when the stage was
    /// built with `FSTART_SMM_IMAGE` set. `None` without an SMM image; MP
    /// setup then skips SMM relocation and SMRAM stays unlocked.
    #[cfg(fstart_qemu_has_smm_image)]
    pub const SMM_IMAGE: Option<&'static [u8]> = Some(include_bytes!(env!("FSTART_SMM_IMAGE")));
    #[cfg(not(fstart_qemu_has_smm_image))]
    pub const SMM_IMAGE: Option<&'static [u8]> = None;

    pub struct QemuQ35Mainstage {
        config: &'static QemuQ35Config,
        console: Ns16550,
        fw_cfg: QemuFwCfg,
        hostbridge: Q35HostBridge,
        e820: E820State,
        acpi_rsdp: Option<u64>,
        framebuffer: Option<fstart_core::services::FramebufferInfo>,
    }

    impl QemuQ35Mainstage {
        fn new<B: QemuQ35Board>() -> Result<Self, ServiceError> {
            Ok(Self {
                config: B::CONFIG,
                console: Ns16550::new(B::console_config())
                    .map_err(|_| ServiceError::HardwareError)?,
                fw_cfg: QemuFwCfg::new(B::CONFIG.fw_cfg),
                hostbridge: Q35HostBridge::new(B::CONFIG.hostbridge)?,
                e820: E820State::new(),
                acpi_rsdp: None,
                framebuffer: None,
            })
        }

        fn init_console<B: QemuQ35Board>(&mut self) -> Result<(), ServiceError> {
            self.console
                .init()
                .map_err(|_| ServiceError::HardwareError)?;
            // SAFETY: the mainstage owns the console until payload handoff.
            unsafe { fstart_log::init(&self.console) };
            fstart_log::info!("{}: ns16550 console ready", B::console_node());
            fstart_log::info!("qemu-q35 ramstage (monolithic) console ready");
            Ok(())
        }

        fn detect_memory_and_tables(&mut self) -> Result<(), ServiceError> {
            self.fw_cfg.init()?;
            let count = self.fw_cfg.detect_memory(self.e820.entries_mut())?;
            let total = self.fw_cfg.total_ram_bytes()?;
            self.e820.set_detected(count, total);
            self.reserve_tseg();
            fstart_log::info!(
                "Detected {} MiB RAM, {} e820 entries from {}",
                self.e820.total_ram() >> 20,
                self.e820.count(),
                QEMU_Q35_PLATFORM_NODE,
            );

            static mut ACPI_BUFFER: [u8; QEMU_Q35_ACPI_BUFFER_SIZE] =
                [0; QEMU_Q35_ACPI_BUFFER_SIZE];
            // SAFETY: firmware init is single-threaded; this buffer is written once.
            let acpi = unsafe { &mut *core::ptr::addr_of_mut!(ACPI_BUFFER) };
            self.acpi_rsdp = Some(self.fw_cfg.load_acpi_tables(acpi)?);
            Ok(())
        }

        fn init_pci(&mut self) -> Result<(), ServiceError> {
            self.hostbridge.init_with_e820(self.e820.entries())?;
            fstart_log::info!(
                "qemu-q35: PCI root ready ({} devices)",
                self.hostbridge.device_count(),
            );
            Ok(())
        }

        fn init_display(&mut self) -> Result<(), ServiceError> {
            // fbuild always passes `-device bochs-display`; program it
            // at 1024x768 when present, ignore when absent (e.g. a
            // custom QEMU command line without the device).
            let info = BochsDisplay::probe_and_init(
                self.hostbridge.ecam(),
                BochsDisplayConfig {
                    width: 1024,
                    height: 768,
                },
            )
            .map_err(|_| ServiceError::HardwareError)?;
            if let Some(info) = info {
                fstart_log::info!(
                    "qemu-q35: display {}x{} fb={:#x}",
                    info.width,
                    info.height,
                    info.base_addr,
                );
                self.framebuffer = Some(info);
            } else {
                fstart_log::info!("qemu-q35: no bochs-display, continuing headless");
            }
            Ok(())
        }

        /// Maximum SMM entry stubs baked into the SMM image (see the `smm`
        /// unit in `host.rs`). QEMU `-smp` above this is clamped.
        const SMM_ENTRY_COUNT: u16 = 8;

        /// Plan-time reservation at the top of low RAM for TSEG. Must cover
        /// any TSEG QEMU may report; see the `ram` span in `host.rs`.
        const TSEG_RESERVE: u64 = 0x0100_0000;

        /// Carve the TSEG window out of the firmware memory map before any
        /// consumer (boot-media mount, PCI MMIO windows, payload e820)
        /// treats it as usable DRAM. QEMU's `etc/e820` does not describe
        /// TSEG: it overlays the top of low RAM, so without this the
        /// overlay would be handed out as ordinary memory.
        fn reserve_tseg(&mut self) {
            let size = crate::q35_smm::decode_tseg_size() as u64;
            if size == 0 {
                return;
            }
            let base = crate::q35_smm::tseg_base_from_e820(self.e820.entries(), size as usize);
            if base == 0 {
                fstart_log::error!("Q35: unable to locate TSEG, leaving map uncarved");
                return;
            }
            self.e820.reserve_range(base, size);
            let total = self.e820.total_ram().saturating_sub(size);
            let count = self.e820.count();
            self.e820.set_detected(count, total);
            fstart_log::info!("Q35: TSEG reserved base={:#x} size={:#x}", base, size);
        }

        /// Bring up APs and, when an SMM image is embedded, relocate SMBASE,
        /// install the permanent TSEG handler, and lock SMRAM.
        ///
        /// Runs after PCI init: TSEG geometry is captured there and ICH9
        /// PMBASE is programmed, both required by the SMM flow.
        fn init_mp_smm(&self) -> Result<(), ServiceError> {
            let cpu = fstart_arch::mp::GenericX86CpuDriver;
            let drivers: [&dyn fstart_arch::mp::CpuDriver; 1] = [&cpu];
            let smm = SMM_IMAGE.map(|_| &self.hostbridge as &dyn fstart_arch::mp::SmmOps);
            if smm.is_some() {
                // Locking SMM hides TSEG from non-SMM access. Firmware
                // statics live below the plan-time reservation by
                // construction; halt loudly instead of corrupting the
                // stack if a larger TSEG is ever decoded.
                let size = crate::q35_smm::decode_tseg_size() as u64;
                let base = self.hostbridge.tseg_base();
                unsafe extern "C" {
                    static _stack_top: u8;
                }
                // SAFETY: `_stack_top` is defined by the platform linker
                // script; only its address is taken.
                let stack_top = unsafe { &_stack_top as *const u8 as u64 };
                if size == 0 || base == 0 || size > Self::TSEG_RESERVE || base < stack_top {
                    fstart_log::error!(
                        "qemu-q35: TSEG {:#x}+{:#x} exceeds reservation (stack top {:#x})",
                        base,
                        size,
                        stack_top,
                    );
                    return Err(ServiceError::InvalidParam);
                }
            }
            // QEMU `-smp` may exceed the baked entry count; clamp so the
            // installer never addresses stubs that do not exist.
            let max_cpus = self.fw_cfg.max_cpus().min(Self::SMM_ENTRY_COUNT).max(1);
            fstart_log::info!("qemu-q35: MP init with {} CPUs", max_cpus);
            fstart_arch::mp::mp_init(&fstart_arch::mp::MpConfig {
                cpu_drivers: &drivers,
                smm,
                smm_image: SMM_IMAGE,
                max_cpus,
            })
            .map(|_| ())
            .map_err(|_| ServiceError::HardwareError)
        }

        fn mount_boot_media(&self) -> Result<(), ServiceError> {
            fstart_arch::x86_64::enable_boot_media_rom_cache();
            use fstart_core::services::memory_detect::E820Kind;
            use fstart_stage::boot::MemoryWindow;
            let mut writable = heapless::Vec::<MemoryWindow, 32>::new();
            let mut reserved = heapless::Vec::<MemoryWindow, 32>::new();
            for entry in self.e820.entries() {
                let window = MemoryWindow {
                    start: entry.addr,
                    size: entry.size,
                };
                if entry.kind == E820Kind::Ram as u32 {
                    writable
                        .push(window)
                        .map_err(|_| ServiceError::InvalidParam)?;
                } else {
                    reserved
                        .push(window)
                        .map_err(|_| ServiceError::InvalidParam)?;
                }
            }
            reserved
                .push(MemoryWindow {
                    start: crate::QEMU_Q35_PAGE_TABLE_ADDR,
                    size: crate::QEMU_Q35_PAGE_TABLE_SIZE,
                })
                .map_err(|_| ServiceError::InvalidParam)?;
            // The FFS image sits at the firmware base with the locator
            // bounding its packed size; the window's erased tail stays out
            // of the authenticated view.
            crate::boot::install_packed(
                &writable,
                &reserved,
                self.config.firmware_base,
                self.config.firmware_size,
            )
        }
    }

    impl X86UefiPayloadContext for QemuQ35Mainstage {
        fn console(&self) -> Option<&dyn Console> {
            Some(&self.console)
        }

        fn e820(&self) -> &[E820Entry] {
            self.e820.entries()
        }

        fn firmware_region(&self) -> (u64, u64) {
            (self.config.firmware_base, self.config.firmware_size)
        }

        fn acpi_rsdp(&self) -> Option<u64> {
            self.acpi_rsdp
        }

        fn pci_root(&self) -> Option<fstart_pci::PciRootInfo> {
            let config = self.config.hostbridge;
            Some(fstart_pci::PciRootInfo {
                ecam_base: config.ecam_base,
                segment: 0,
                bus_start: config.bus_start,
                bus_end: config.bus_end,
            })
        }

        #[cfg(feature = "crabefi")]
        fn framebuffer(&self) -> Option<fstart_stage::crabefi::FramebufferConfig> {
            self.framebuffer
                .map(|info| fstart_stage::crabefi::FramebufferConfig {
                    physical_address: info.base_addr,
                    width: info.width,
                    height: info.height,
                    stride: info.stride,
                    bits_per_pixel: info.bits_per_pixel,
                    red_mask_pos: info.red_pos,
                    red_mask_size: info.red_size,
                    green_mask_pos: info.green_pos,
                    green_mask_size: info.green_size,
                    blue_mask_pos: info.blue_pos,
                    blue_mask_size: info.blue_size,
                })
        }
    }

    fn phase(name: &str, f: impl FnOnce() -> Result<(), ServiceError>) {
        fstart_log::info!("qemu-q35 ramstage: {}", name);
        if f().is_err() {
            fstart_log::error!("qemu-q35 ramstage: {} failed", name);
            fstart_arch::x86_64::halt();
        }
    }

    pub fn run_qemu_q35_mainstage<B>() -> !
    where
        B: QemuQ35Board,
    {
        let Ok(mut mainstage) = QemuQ35Mainstage::new::<B>() else {
            fstart_arch::x86_64::halt();
        };
        phase("console", || mainstage.init_console::<B>());
        phase("memory_detect", || mainstage.detect_memory_and_tables());
        phase("mount_boot_media", || mainstage.mount_boot_media());
        phase("bus_scan", || mainstage.init_pci());
        phase("display", || mainstage.init_display());
        phase("mp_smm", || mainstage.init_mp_smm());
        phase("finalize", || {
            fstart_log::info!("qemu-q35 ramstage: ready for payload");
            Ok(())
        });
        B::Payload::boot(mainstage)
    }

    impl QemuQ35 {
        pub fn run_stage<B>(env: StageEnvironment, _handoff: usize) -> !
        where
            B: QemuQ35Board,
        {
            if !matches!(env, StageEnvironment::Monolithic) {
                fstart_arch::halt();
            }
            run_qemu_q35_mainstage::<B>()
        }
    }
}
