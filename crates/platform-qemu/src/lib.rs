//! QEMU platform flows and QEMU-only hardware blocks.

#![no_std]

#[cfg(any(feature = "host", feature = "stage"))]
extern crate alloc;
extern crate ufmt;

#[cfg(feature = "stage")]
mod boot;
#[cfg(any(
    test,
    all(
        feature = "stage",
        any(target_arch = "arm", target_arch = "aarch64", target_arch = "riscv64")
    )
))]
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
/// Declare a board's stage `main`: `fstart_platform_qemu::entry!(main);`.
/// RISC-V boards that can launch CrabEFI also name the function OpenSBI
/// returns to: `entry!(main, sbi_resume: resume);`.
#[macro_export]
macro_rules! entry {
    ($main:path) => { $crate::stage_runtime::stage_bin!(main: $main); };
    ($main:path, sbi_resume: $resume:path) => {
        $crate::stage_runtime::stage_bin!(main: $main, sbi_resume: $resume);
    };
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
pub use sbsa::QemuSbsaMainstage;
#[cfg(all(feature = "stage", feature = "riscv64", target_arch = "riscv64"))]
pub use sifive_u::{QemuSifiveUBuildSelectedPayload, QemuSifiveUMainstage};
#[cfg(all(feature = "stage", feature = "virt-aarch64", target_arch = "aarch64"))]
pub use virt_aarch64::QemuAarch64VirtMainstage;
#[cfg(all(feature = "stage", feature = "armv7", target_arch = "arm"))]
pub use virt_armv7::QemuArmv7VirtMainstage;
#[cfg(all(feature = "stage", feature = "virt-riscv64", target_arch = "riscv64"))]
pub use virt_riscv64::QemuRiscv64VirtMainstage;
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub mod q35;
#[cfg(all(
    feature = "smm",
    any(feature = "stage", test),
    any(target_arch = "x86", target_arch = "x86_64")
))]
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
pub use stage::{QemuQ35Mainstage, run_q35};

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
    use fstart_core::services::ServiceError;
    use fstart_core::services::memory_detect::E820State;
    use fstart_driver_uart::ns16550::{Ns16550, Ns16550Config};
    use fstart_stage::payload::MainstagePayload;

    use crate::fw_cfg::QemuFwCfg;
    use crate::q35::Q35HostBridge;
    use crate::{QEMU_Q35_ACPI_BUFFER_SIZE, QEMU_Q35_PLATFORM_NODE, QemuQ35Config};

    #[cfg(not(fstart_entry = "x86_64"))]
    compile_error!("QEMU q35 requires the x86_64 entry");
    #[cfg(not(any(
        fstart_payload = "halt",
        fstart_payload = "crabefi",
        fstart_payload = "coreboot"
    )))]
    compile_error!("select exactly one q35 payload");
    #[cfg(all(fstart_payload = "crabefi", not(feature = "crabefi")))]
    compile_error!("selected CrabEFI backend is not enabled");

    /// Native SMM handler image built by fbuild, embedded when the stage was
    /// built with `FSTART_SMM_IMAGE` set. `None` without an SMM image; MP
    /// setup then skips SMM relocation and SMRAM stays unlocked.
    #[cfg(fstart_qemu_has_smm_image)]
    pub const SMM_IMAGE: Option<&'static [u8]> = Some(include_bytes!(env!("FSTART_SMM_IMAGE")));
    #[cfg(not(fstart_qemu_has_smm_image))]
    pub const SMM_IMAGE: Option<&'static [u8]> = None;

    // The table loader's ALLOCATE alignments (64 for the tables blob, 16 for
    // the RSDP) are applied to offsets in this buffer, so the buffer itself
    // must be at least as aligned as any request.
    #[repr(C, align(4096))]
    struct AcpiBuffer([u8; QEMU_Q35_ACPI_BUFFER_SIZE]);
    static mut ACPI_BUFFER: AcpiBuffer = AcpiBuffer([0; QEMU_Q35_ACPI_BUFFER_SIZE]);

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
        fn new(
            config: &'static QemuQ35Config,
            console: Ns16550Config,
        ) -> Result<Self, ServiceError> {
            Ok(Self {
                config,
                console: Ns16550::new(console).map_err(|_| ServiceError::HardwareError)?,
                fw_cfg: QemuFwCfg::new(config.fw_cfg),
                hostbridge: Q35HostBridge::new(config.hostbridge)?,
                e820: E820State::new(),
                acpi_rsdp: None,
                framebuffer: None,
            })
        }

        fn init_console(&mut self) -> Result<(), ServiceError> {
            self.console
                .init()
                .map_err(|_| ServiceError::HardwareError)?;
            // SAFETY: the mainstage owns the console until payload handoff.
            unsafe { fstart_log::init(&self.console) };
            fstart_log::info!("qemu-q35 ramstage (monolithic) console ready");
            Ok(())
        }

        fn detect_memory_and_tables(&mut self) -> Result<(), ServiceError> {
            self.fw_cfg.init()?;
            let count = self.fw_cfg.detect_memory(self.e820.entries_mut())?;
            let total = self.fw_cfg.total_ram_bytes()?;
            self.e820.set_detected(count, total)?;
            self.reserve_tseg()?;
            fstart_log::info!(
                "Detected {} MiB RAM, {} e820 entries from {}",
                self.e820.total_ram() >> 20,
                self.e820.count(),
                QEMU_Q35_PLATFORM_NODE,
            );

            // QEMU fills the FADT PM blocks from PMBASE when it builds the
            // tables on this first read.
            self.hostbridge.setup_ich9_pm_io()?;
            // SAFETY: firmware init is single-threaded; this buffer is written once.
            let acpi = unsafe { &mut (*core::ptr::addr_of_mut!(ACPI_BUFFER)).0 };
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

        /// Plan-time reservation at the top of low RAM for TSEG. Must cover
        /// any TSEG QEMU may report; see the `ram` span in `host.rs`.
        const TSEG_RESERVE: u64 = 0x0100_0000;

        /// Carve the TSEG window out of the firmware memory map before any
        /// consumer (boot-media mount, PCI MMIO windows, payload e820)
        /// treats it as usable DRAM. QEMU's `etc/e820` does not describe
        /// TSEG: it overlays the top of low RAM, so without this the
        /// overlay would be handed out as ordinary memory.
        fn reserve_tseg(&mut self) -> Result<(), ServiceError> {
            // Geometry discovery precedes PCI allocation; enable the shared
            // config accessor here instead of keeping an early CF8 copy.
            self.hostbridge.enable_config_access()?;
            let size = crate::q35_smm::decode_tseg_size() as u64;
            if size == 0 {
                return Ok(());
            }
            let base = crate::q35_smm::tseg_base_from_e820(self.e820.entries(), size as usize);
            if base == 0 {
                fstart_log::error!("Q35: unable to locate TSEG");
                return Err(ServiceError::HardwareError);
            }
            self.e820.reserve_range(base, size)?;
            let total = self.e820.total_ram().saturating_sub(size);
            let count = self.e820.count();
            self.e820.set_detected(count, total)?;
            fstart_log::info!("Q35: TSEG reserved base={:#x} size={:#x}", base, size);
            Ok(())
        }

        /// Bring up APs and, when an SMM image is embedded, relocate SMBASE,
        /// install the permanent TSEG handler, and lock SMRAM.
        ///
        /// Runs after PCI init: TSEG geometry is captured there and ICH9
        /// PMBASE is programmed, both required by the SMM flow.
        /// Identity map RAM above 4 GiB and the assigned PCI BARs: the entry
        /// tables cover only 4 GiB without 1 GiB pages. Before AP startup,
        /// which inherits this CR3.
        #[cfg(target_arch = "x86_64")]
        fn map_memory(&self) -> Result<(), ServiceError> {
            use fstart_core::services::memory_detect::E820Kind;
            let ram = self
                .e820
                .entries()
                .iter()
                .filter(|entry| entry.kind == E820Kind::Ram as u32)
                .map(|entry| (entry.addr, entry.size));
            let bars = self
                .hostbridge
                .ecam()
                .assigned_memory_windows()
                .map(|window| (window.base, window.size));
            // SAFETY: BSP only, before APs and the payload; the frames come
            // from the firmware heap, identity mapped by the entry tables.
            let tables =
                unsafe { fstart_arch::x86::boot::paging::IdentityTables::build(ram.chain(bars))? };
            let base = unsafe { tables.install() };
            fstart_log::info!("qemu-q35: identity tables at {:#x}", base);
            Ok(())
        }

        fn init_mp_smm(&self) -> Result<(), ServiceError> {
            let cpu = fstart_arch::x86::mp::GenericX86CpuDriver;
            let drivers: [&dyn fstart_arch::x86::mp::CpuDriver; 1] = [&cpu];
            if SMM_IMAGE.is_some() {
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
            // Only an embedded SMM image constrains MP bring-up by its entry
            // count. Do not impose an SMM-specific limit on non-SMM builds.
            let num_cpus = self.fw_cfg.cpu_count();
            let max_cpus = if let Some(image) = SMM_IMAGE {
                let header = fstart_smm::header::SmmImageHeader::parse(image)
                    .map_err(|_| ServiceError::InvalidParam)?;
                header.entry_count
            } else {
                num_cpus
            };
            fstart_log::info!("qemu-q35: MP init with {} CPUs", num_cpus);
            let mp = fstart_arch::x86::mp::mp_init(&fstart_arch::x86::mp::MpConfig {
                cpu_drivers: &drivers,
                num_cpus,
                max_cpus,
            })
            .map_err(|_| ServiceError::HardwareError)?;
            if let Some(image) = SMM_IMAGE {
                #[cfg(feature = "tpm")]
                fstart_tcg::measure_blob(
                    0,
                    fstart_tcg::event::EV_POST_CODE2,
                    fstart_tcg::event::post_code::SMM_CODE,
                    image.as_ptr() as u64,
                    image,
                );
                let smi = crate::q35_smm::ich9_smi();
                let smm = fstart_arch::x86::cpu::intel::smm::IntelSmm::new(
                    "Q35",
                    &self.hostbridge,
                    &smi,
                    &crate::q35_smm::QemuSmmCpu,
                    false,
                );
                if let Err(error) = smm.install(&mp, image) {
                    fstart_log::error!("Q35 SMM installation failed: {}", error.name());
                    return Err(ServiceError::HardwareError);
                }
            }
            Ok(())
        }

        /// Start measured boot when QEMU has a TPM: its TPM2 or TCPA table
        /// then points at the `etc/tpm/log` area loaded into the ACPI
        /// buffer, which becomes the event log. The TPM must speak the FIFO
        /// interface (`tpm-tis`).
        #[cfg(feature = "tpm")]
        fn init_tpm(&mut self) -> Result<(), ServiceError> {
            use fstart_driver_tpm::tis::{self, Tis};
            let Some((offset, size)) = crate::fw_cfg::acpi_allocation("etc/tpm/log") else {
                fstart_log::info!("qemu-q35: no TPM");
                return Ok(());
            };
            static mut TIS: Option<Tis<fstart_arch::x86::Udelay>> = None;
            // SAFETY: QEMU decodes the TIS window when it describes a TPM,
            // and nothing else drives it.
            let tis = unsafe { Tis::new(tis::BASE, fstart_arch::x86::Udelay) };
            let Some(id) = tis.probe() else {
                fstart_log::warn!("qemu-q35: TPM tables, but no FIFO TPM at {:#x}", tis::BASE);
                return Ok(());
            };
            if tis.is_crb() {
                fstart_log::warn!("qemu-q35: CRB TPMs are not supported (use tpm-tis)");
                return Ok(());
            }
            fstart_log::info!(
                "qemu-q35: TPM {:#x}:{:#x} revision {}",
                id.vendor,
                id.device,
                id.revision
            );
            // SAFETY: single-threaded init; the slot is written once.
            let tis = unsafe { (*core::ptr::addr_of_mut!(TIS)).insert(tis) };
            // SAFETY: the table loader placed the log area inside
            // ACPI_BUFFER, a firmware static reserved from the OS; nothing
            // else references it.
            let log = unsafe {
                core::slice::from_raw_parts_mut(
                    (core::ptr::addr_of_mut!(ACPI_BUFFER) as *mut u8).add(offset),
                    size,
                )
            };
            let uefi = cfg!(fstart_payload = "crabefi");
            match fstart_tcg::start(tis, log, fstart_tcg::PlatformClass::Client, uefi) {
                Ok(_) => fstart_stage::tcg::measure_srtm(fstart_stage::tcg::CRTM_VERSION, "stage"),
                Err(error) => {
                    error.report();
                    if error.remedy() == fstart_tcg::Remedy::Halt {
                        return Err(ServiceError::HardwareError);
                    }
                    // QEMU's own ACPI describes the TPM: it cannot be hidden.
                    fstart_log::warn!("qemu-q35: measured boot off");
                }
            }
            Ok(())
        }

        fn mount_boot_media(&self) -> Result<(), ServiceError> {
            fstart_arch::x86::boot::enable_boot_media_rom_cache();
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

    /// RAM for the coreboot table and the files handed to the payload.
    #[cfg(fstart_payload = "coreboot")]
    const COREBOOT_WINDOW_SIZE: usize = 0x4_0000;
    #[cfg(fstart_payload = "coreboot")]
    #[repr(C, align(4096))]
    struct CorebootWindow([u8; COREBOOT_WINDOW_SIZE]);
    #[cfg(fstart_payload = "coreboot")]
    static mut COREBOOT_WINDOW: CorebootWindow = CorebootWindow([0; COREBOOT_WINDOW_SIZE]);

    impl QemuQ35Mainstage {
        /// Describe the machine to the build's payload launcher and boot it.
        fn hand_off(&self) -> ! {
            use fstart_core::services::ConsoleDevice;
            use fstart_stage::payload::{
                AcpiTables, BuildSelectedPayload, Description, Handoff, PciRootInfo, Placement,
                Range,
            };
            let acpi_buffer = Range::new(
                core::ptr::addr_of!(ACPI_BUFFER) as u64,
                QEMU_Q35_ACPI_BUFFER_SIZE as u64,
            );
            let tables = [acpi_buffer];
            let config = self.config.hostbridge;
            let mut description = Description::new(
                self.e820.entries(),
                Range::new(self.config.firmware_base, self.config.firmware_size),
            );
            description.console = Some(&self.console);
            description.uart = self.console.uart_port();
            description.pci_root = Some(PciRootInfo {
                ecam_base: config.ecam_base,
                segment: 0,
                bus_start: config.bus_start,
                bus_end: config.bus_end,
            });
            description.framebuffer = self.framebuffer;
            description.acpi = self.acpi_rsdp.map(|rsdp| AcpiTables {
                rsdp,
                region: acpi_buffer,
            });
            description.mainboard = ("QEMU", "Q35");
            description.tables = &tables;
            #[allow(unused_mut)]
            let mut placement = Placement::default();
            // SAFETY: fstart programs PAM0-6 to DRAM during PCI init and keeps
            // nothing in the legacy shadow; the window is a firmware static
            // that only the launcher touches.
            #[cfg(fstart_payload = "coreboot")]
            {
                placement.coreboot_window = Some(unsafe {
                    fstart_stage::coreboot::Window::new(
                        core::ptr::addr_of!(COREBOOT_WINDOW) as u64,
                        COREBOOT_WINDOW_SIZE,
                    )
                });
            }
            BuildSelectedPayload::boot(Handoff::new(description, placement), ())
        }
    }

    fn phase(name: &str, f: impl FnOnce() -> Result<(), ServiceError>) {
        fstart_log::info!("qemu-q35 ramstage: {}", name);
        if f().is_err() {
            fstart_log::error!("qemu-q35 ramstage: {} failed", name);
            fstart_arch::x86::boot::halt();
        }
    }

    /// The QEMU q35 stage: console, memory map and ACPI from fw_cfg, boot
    /// media, PCI, display, MP and SMM, then the payload. QEMU boards have
    /// no board work; their `main` calls this.
    pub fn run_q35(config: &'static QemuQ35Config, console: Ns16550Config) -> ! {
        fstart_stage::anchor::initial_stage();
        let Ok(mut mainstage) = QemuQ35Mainstage::new(config, console) else {
            fstart_arch::x86::boot::halt();
        };
        phase("console", || mainstage.init_console());
        phase("memory_detect", || mainstage.detect_memory_and_tables());
        phase("mount_boot_media", || mainstage.mount_boot_media());
        #[cfg(feature = "tpm")]
        phase("tpm", || mainstage.init_tpm());
        phase("bus_scan", || mainstage.init_pci());
        phase("display", || mainstage.init_display());
        #[cfg(target_arch = "x86_64")]
        phase("page_tables", || mainstage.map_memory());
        phase("mp_smm", || mainstage.init_mp_smm());
        phase("finalize", || {
            fstart_log::info!("qemu-q35 ramstage: ready for payload");
            Ok(())
        });
        mainstage.hand_off()
    }
}
