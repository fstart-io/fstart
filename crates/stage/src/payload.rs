//! Common payload launch abstractions for mainstage flows.

use fstart_core::services::memory_detect::E820Entry;

/// Build-selected payload launcher for a mainstage device context.
pub trait MainstagePayload<D> {
    /// Consume initialized mainstage devices and transfer control to the payload.
    fn boot(devices: D) -> !;
}

/// Payload launcher used when no payload backend is linked.
pub struct HaltPayload;

impl<D> MainstagePayload<D> for HaltPayload {
    fn boot(_devices: D) -> ! {
        loop {
            core::hint::spin_loop();
        }
    }
}

/// Small platform context consumed by the common Linux payload launcher.
#[cfg(feature = "linux")]
#[derive(Debug, Clone, Copy)]
pub struct LinuxPayloadConfig {
    pub firmware_base: u64,
    pub firmware_size: u64,
    pub source_dtb_addr: u64,
    pub dtb_addr: u64,
    pub kernel_addr: u64,
    pub firmware_addr: u64,
    pub ram_base: u64,
    pub ram_size: u64,
    pub hart_id: u64,
    pub bootargs: &'static str,
}

#[cfg(feature = "linux")]
impl LinuxPayloadConfig {
    #[must_use]
    pub const fn new(
        firmware_base: u64,
        firmware_size: u64,
        source_dtb_addr: u64,
        dtb_addr: u64,
        kernel_addr: u64,
        firmware_addr: u64,
        ram_base: u64,
        ram_size: u64,
        hart_id: u64,
        bootargs: &'static str,
    ) -> Self {
        Self {
            firmware_base,
            firmware_size,
            source_dtb_addr,
            dtb_addr,
            kernel_addr,
            firmware_addr,
            ram_base,
            ram_size,
            hart_id,
            bootargs,
        }
    }
}

/// Platform context source for the common Linux payload launcher.
#[cfg(feature = "linux")]
pub trait LinuxPayloadContext {
    fn linux_payload_context(&self) -> LinuxPayloadConfig;
}

/// Common Linux payload launcher.
#[cfg(feature = "linux")]
pub struct LinuxPayload;

#[cfg(feature = "linux")]
impl<D: LinuxPayloadContext> MainstagePayload<D> for LinuxPayload {
    fn boot(devices: D) -> ! {
        use crate::fixed_helpers::MemoryMappedLinuxBoot;

        let config = devices.linux_payload_context();
        let mut boot = MemoryMappedLinuxBoot::new(
            config.firmware_base,
            config.firmware_size as usize,
            config.source_dtb_addr,
        );
        if boot.mount().is_err()
            || boot.verify().is_err()
            || if config.firmware_addr == 0 {
                boot.load_kernel().is_err()
            } else {
                boot.load_firmware_and_kernel().is_err()
            }
            || boot
                .prepare_fdt(
                    config.dtb_addr,
                    config.bootargs,
                    config.ram_base,
                    config.ram_size,
                )
                .is_err()
        {
            fstart_log::error!("Linux payload: FFS payload load failed");
            halt_linux();
        }

        let Ok(params) = boot.checked_boot_params(
            config.kernel_addr,
            config.firmware_addr,
            config.hart_id,
            config.bootargs,
        ) else {
            fstart_log::error!("Linux payload: configured entry was not verified");
            halt_linux();
        };
        launch_linux(&params)
    }
}

#[cfg(feature = "linux")]
fn halt_linux() -> ! {
    #[cfg(feature = "riscv64")]
    fstart_arch::riscv64::halt();
    #[cfg(feature = "aarch64")]
    fstart_arch::aarch64::halt();
    #[cfg(feature = "armv7")]
    fstart_arch::halt();
    #[cfg(not(any(feature = "riscv64", feature = "aarch64", feature = "armv7")))]
    loop {
        core::hint::spin_loop();
    }
}

#[cfg(feature = "linux")]
fn launch_linux(params: &fstart_core::services::BootLinuxParams<'_>) -> ! {
    #[cfg(feature = "riscv64")]
    fstart_arch::riscv64::boot_linux(params);
    #[cfg(feature = "aarch64")]
    fstart_arch::aarch64::boot_linux(params);
    #[cfg(feature = "armv7")]
    fstart_arch::armv7::boot_linux(params);
    #[cfg(not(any(feature = "riscv64", feature = "aarch64", feature = "armv7")))]
    {
        let _ = params;
        halt_linux()
    }
}

/// Small platform context consumed by the AArch64 UEFI payload launcher.
#[cfg(all(feature = "crabefi-basic", feature = "aarch64"))]
#[derive(Debug, Clone, Copy)]
pub struct Aarch64UefiPayloadConfig {
    pub flash_base: u64,
    pub flash_size: u64,
    pub firmware_base: u64,
    pub firmware_size: u64,
    pub source_dtb_addr: u64,
    pub firmware_addr: u64,
    pub ram_base: u64,
    pub ram_size: u64,
    pub pci_root: Option<fstart_pci::PciRootInfo>,
}

#[cfg(all(feature = "crabefi-basic", feature = "aarch64"))]
impl Aarch64UefiPayloadConfig {
    #[must_use]
    pub const fn new(
        flash_base: u64,
        flash_size: u64,
        firmware_base: u64,
        firmware_size: u64,
        source_dtb_addr: u64,
        firmware_addr: u64,
        ram_base: u64,
        ram_size: u64,
        pci_root: Option<fstart_pci::PciRootInfo>,
    ) -> Self {
        Self {
            flash_base,
            flash_size,
            firmware_base,
            firmware_size,
            source_dtb_addr,
            firmware_addr,
            ram_base,
            ram_size,
            pci_root,
        }
    }
}

/// Platform context source for the AArch64 UEFI payload launcher.
#[cfg(all(feature = "crabefi-basic", feature = "aarch64"))]
pub trait Aarch64UefiPayloadContext {
    fn aarch64_uefi_payload_context(&self) -> Aarch64UefiPayloadConfig;
    fn console(&self) -> &dyn fstart_core::services::Console;
}

/// AArch64 UEFI launcher for a memory-mapped FFS and BL31.
#[cfg(all(feature = "crabefi-basic", feature = "aarch64"))]
pub struct Aarch64UefiPayload;

#[cfg(all(feature = "crabefi-basic", feature = "aarch64"))]
impl<D: Aarch64UefiPayloadContext> MainstagePayload<D> for Aarch64UefiPayload {
    fn boot(devices: D) -> ! {
        use crate::crabefi::{MemoryRegion, MemoryType, UefiLaunchConfig};
        use crate::fixed_helpers::MemoryMappedUefiBoot;

        let config = devices.aarch64_uefi_payload_context();
        let mut boot = MemoryMappedUefiBoot::new(
            config.firmware_base,
            config.firmware_size as usize,
            config.source_dtb_addr,
        );
        if boot.mount().is_err() || boot.verify().is_err() || boot.load_firmware().is_err() {
            fstart_log::error!("AArch64 UEFI payload: BL31 load failed");
            fstart_arch::aarch64::halt();
        }

        // SAFETY: QEMU supplied this FDT and it remains valid through CrabEFI.
        let fdt = unsafe { boot.fdt_bytes() };
        // SAFETY: QEMU supplied the FDT header at the same stable address.
        let fdt_reservation = unsafe { boot.fdt_reservation() };
        let Ok(entry) = boot.checked_firmware_entry(config.firmware_addr) else {
            fstart_log::error!("AArch64 UEFI payload: configured BL31 entry was not verified");
            fstart_arch::aarch64::halt();
        };
        fstart_arch::aarch64::boot_bl31_and_resume(entry, boot.fdt_addr());
        fstart_log::info!("aarch64 uefi: BL31 returned, launching CrabEFI");

        let ecam = uefi_ecam(config.pci_root);
        let static_entries = [MemoryRegion {
            base: config.flash_base,
            size: config.flash_size,
            region_type: MemoryType::Reserved,
        }];
        crate::crabefi::launch_flat_uefi(
            UefiLaunchConfig {
                console: Some(devices.console()),
                framebuffer: None,
                acpi_rsdp: None,
                smbios: None,
                fdt,
                ecam_regions: ecam.as_slice(),
            },
            &static_entries,
            config.ram_base,
            config.ram_size,
            fdt_reservation,
        )
    }
}

/// Small platform context consumed by the RISC-V UEFI payload launcher.
#[cfg(all(feature = "crabefi-basic", feature = "riscv64"))]
#[derive(Debug, Clone, Copy)]
pub struct Riscv64UefiPayloadConfig {
    pub firmware_base: u64,
    pub firmware_size: u64,
    pub firmware_addr: u64,
    pub firmware_reserve_size: u64,
    pub dtb_addr: u64,
    pub ram_base: u64,
    pub ram_size: u64,
    pub pci_root: Option<fstart_pci::PciRootInfo>,
}

#[cfg(all(feature = "crabefi-basic", feature = "riscv64"))]
impl Riscv64UefiPayloadConfig {
    #[must_use]
    pub const fn new(
        firmware_base: u64,
        firmware_size: u64,
        firmware_addr: u64,
        firmware_reserve_size: u64,
        dtb_addr: u64,
        ram_base: u64,
        ram_size: u64,
        pci_root: Option<fstart_pci::PciRootInfo>,
    ) -> Self {
        Self {
            firmware_base,
            firmware_size,
            firmware_addr,
            firmware_reserve_size,
            dtb_addr,
            ram_base,
            ram_size,
            pci_root,
        }
    }
}

/// Platform context source for the RISC-V UEFI payload launcher.
#[cfg(all(feature = "crabefi-basic", feature = "riscv64"))]
pub trait Riscv64UefiPayloadContext {
    fn riscv64_uefi_payload_context(&self) -> Riscv64UefiPayloadConfig;
    fn console(&self) -> &dyn fstart_core::services::Console;
}

/// RISC-V UEFI launcher using OpenSBI for the S-mode transition.
#[cfg(all(feature = "crabefi-basic", feature = "riscv64"))]
pub struct Riscv64UefiPayload;

#[cfg(all(feature = "crabefi-basic", feature = "riscv64"))]
impl Riscv64UefiPayload {
    /// Load OpenSBI, then ask it to resume the board-selected S-mode entry.
    pub fn boot<D: Riscv64UefiPayloadContext>(devices: D) -> ! {
        use crate::fixed_helpers::MemoryMappedUefiBoot;

        let config = devices.riscv64_uefi_payload_context();
        let dtb_addr = fstart_arch::riscv64::boot_dtb_addr();
        let mut boot = MemoryMappedUefiBoot::new(
            config.firmware_base,
            config.firmware_size as usize,
            dtb_addr,
        );
        if boot.mount().is_err() || boot.verify().is_err() || boot.load_firmware().is_err() {
            fstart_log::error!("RISC-V UEFI payload: OpenSBI load failed");
            fstart_arch::riscv64::halt();
        }

        let info = fstart_arch::riscv64::FwDynamicInfo::new(
            fstart_arch::riscv64::sbi_resume_trampoline(),
            fstart_arch::riscv64::boot_hart_id(),
        );
        let Ok(entry) = boot.checked_firmware_entry(config.firmware_addr) else {
            fstart_log::error!("RISC-V UEFI payload: configured OpenSBI entry was not verified");
            fstart_arch::riscv64::halt();
        };
        // OpenSBI adds DTB properties/reservations. Give it the dedicated
        // writable workspace, not QEMU's exactly bounded source blob. On
        // return, copy_fdt_to_workspace accepts growth only within that same
        // registered capacity; do not relax the original source bound.
        let Ok(used) = crate::copy_fdt_to_workspace(boot.fdt_addr(), config.dtb_addr) else {
            fstart_log::error!("RISC-V UEFI payload: bounded FDT preparation failed");
            fstart_arch::riscv64::halt();
        };
        let capacity = crate::fdt_workspace::destination().map_or(0, |r| r.size);
        // Keep working room for the supported OpenSBI image's DTB fixups.
        if (used as u64)
            .checked_add(8192)
            .is_none_or(|end| end > capacity)
        {
            fstart_log::error!("RISC-V UEFI payload: insufficient FDT workspace slack");
            fstart_arch::riscv64::halt();
        }
        fstart_arch::riscv64::boot_sbi(
            entry,
            fstart_arch::riscv64::boot_hart_id(),
            config.dtb_addr,
            &info,
        )
    }

    /// Enter CrabEFI after OpenSBI has switched the processor to S-mode.
    pub fn resume_sbi<D: Riscv64UefiPayloadContext>(devices: D, hart_id: u64, dtb_addr: u64) -> ! {
        use crate::crabefi::{MemoryRegion, MemoryType, UefiLaunchConfig};

        let config = devices.riscv64_uefi_payload_context();
        let Ok(fdt_size) = crate::copy_fdt_to_workspace(dtb_addr, config.dtb_addr) else {
            fstart_log::error!("RISC-V UEFI payload: bounded FDT copy failed");
            fstart_arch::riscv64::halt();
        };
        // SAFETY: bounded copy validated and initialized the dedicated workspace;
        // this flow makes no further mutations before passing it to CrabEFI.
        let fdt =
            Some(unsafe { core::slice::from_raw_parts(config.dtb_addr as *const u8, fdt_size) });
        let fdt_reservation =
            fdt.map(|bytes| (config.dtb_addr, (bytes.len() as u64 + 0xfff) & !0xfff));
        crate::crabefi::set_riscv_boot_hartid(hart_id);

        let ecam = uefi_ecam(config.pci_root);
        let static_entries = [
            MemoryRegion {
                base: config.firmware_addr,
                size: config.firmware_reserve_size,
                region_type: MemoryType::Reserved,
            },
            MemoryRegion {
                base: 0x1000_0000,
                size: 0x100,
                region_type: MemoryType::Mmio,
            },
            MemoryRegion {
                base: 0x0200_0000,
                size: 0x1_0000,
                region_type: MemoryType::Mmio,
            },
            MemoryRegion {
                base: 0x0c00_0000,
                size: 0x0400_0000,
                region_type: MemoryType::Mmio,
            },
            MemoryRegion {
                base: 0x4000_0000,
                size: 0x4000_0000,
                region_type: MemoryType::Mmio,
            },
        ];
        crate::crabefi::launch_flat_uefi(
            UefiLaunchConfig {
                console: Some(devices.console()),
                framebuffer: None,
                acpi_rsdp: None,
                smbios: None,
                fdt,
                ecam_regions: ecam.as_slice(),
            },
            &static_entries,
            config.ram_base,
            config.ram_size,
            fdt_reservation,
        )
    }
}

#[cfg(all(feature = "crabefi-basic", feature = "riscv64"))]
impl<D: Riscv64UefiPayloadContext> MainstagePayload<D> for Riscv64UefiPayload {
    fn boot(devices: D) -> ! {
        Self::boot(devices)
    }
}

/// Payload launcher selected by fbuild features.
#[cfg(all(feature = "crabefi-basic", feature = "aarch64"))]
pub type BuildSelectedPayload = Aarch64UefiPayload;

/// Payload launcher selected by fbuild features.
#[cfg(all(feature = "crabefi-basic", feature = "riscv64"))]
pub type BuildSelectedPayload = Riscv64UefiPayload;

/// Payload launcher selected by fbuild features.
#[cfg(all(feature = "crabefi-basic", feature = "x86_64"))]
pub type BuildSelectedPayload = X86UefiPayload;

/// Payload launcher selected by fbuild features.
#[cfg(all(not(feature = "crabefi-basic"), feature = "linux"))]
pub type BuildSelectedPayload = LinuxPayload;

/// Payload launcher used when fbuild selected no payload backend.
#[cfg(all(not(feature = "crabefi-basic"), not(feature = "linux")))]
pub type BuildSelectedPayload = HaltPayload;

/// Device context needed by the common x86 CrabEFI launcher.
pub trait X86UefiPayloadContext {
    /// Return the payload console, if one is available.
    fn console(&self) -> Option<&dyn fstart_core::services::Console>;
    /// Return the detected x86 memory map.
    fn e820(&self) -> &[E820Entry];
    /// Return the firmware media region to keep out of RAM allocation.
    fn firmware_region(&self) -> (u64, u64);
    /// Return the ACPI RSDP physical address, if ACPI was emitted.
    fn acpi_rsdp(&self) -> Option<u64>;
    /// Return the SMBIOS entry point physical address, if SMBIOS was emitted.
    fn smbios(&self) -> Option<u64> {
        None
    }
    /// Return the actual PCI ECAM segment/bus bounds, if available.
    fn pci_root(&self) -> Option<fstart_pci::PciRootInfo>;
    /// Return the programmed linear framebuffer for UEFI GOP, if any.
    #[cfg(feature = "crabefi-basic")]
    fn framebuffer(&self) -> Option<crate::crabefi::FramebufferConfig> {
        None
    }
}

/// Common x86 CrabEFI payload launcher.
#[cfg(all(feature = "crabefi-basic", feature = "x86_64"))]
pub struct X86UefiPayload;

#[cfg(all(feature = "crabefi-basic", feature = "x86_64"))]
impl<D> MainstagePayload<D> for X86UefiPayload
where
    D: X86UefiPayloadContext,
{
    fn boot(devices: D) -> ! {
        use crate::crabefi::{MemoryRegion, MemoryType, UefiLaunchConfig};

        let (firmware_base, firmware_size) = devices.firmware_region();
        let acpi_base = devices.acpi_rsdp().unwrap_or(0) & !0xfff;
        let platform_entries = [
            MemoryRegion {
                base: firmware_base,
                size: firmware_size,
                region_type: MemoryType::Reserved,
            },
            MemoryRegion {
                base: acpi_base,
                size: 0x10000,
                region_type: MemoryType::AcpiReclaimable,
            },
        ];

        let ecam = uefi_ecam(devices.pci_root());
        crate::crabefi::launch_x86_uefi(
            UefiLaunchConfig {
                console: devices.console(),
                framebuffer: devices.framebuffer(),
                acpi_rsdp: devices.acpi_rsdp(),
                smbios: devices.smbios(),
                fdt: None,
                ecam_regions: ecam.as_slice(),
            },
            devices.e820(),
            &platform_entries,
        )
    }
}

#[cfg(feature = "crabefi-basic")]
fn uefi_ecam(root: Option<fstart_pci::PciRootInfo>) -> Option<crate::crabefi::PciEcamRegion> {
    root.map(|root| crate::crabefi::PciEcamRegion {
        base: root.ecam_base,
        segment: root.segment,
        bus_start: root.bus_start,
        bus_end: root.bus_end,
    })
}
