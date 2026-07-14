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

        let params = boot.boot_params(
            config.kernel_addr,
            config.firmware_addr,
            config.hart_id,
            config.bootargs,
        );
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
#[cfg(all(feature = "crabefi", feature = "aarch64"))]
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
    pub firmware_data_addr: u64,
    pub firmware_stack_size: u64,
    pub ecam_base: u64,
}

#[cfg(all(feature = "crabefi", feature = "aarch64"))]
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
        firmware_data_addr: u64,
        firmware_stack_size: u64,
        ecam_base: u64,
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
            firmware_data_addr,
            firmware_stack_size,
            ecam_base,
        }
    }
}

/// Platform context source for the AArch64 UEFI payload launcher.
#[cfg(all(feature = "crabefi", feature = "aarch64"))]
pub trait Aarch64UefiPayloadContext {
    fn aarch64_uefi_payload_context(&self) -> Aarch64UefiPayloadConfig;
    fn console(&self) -> &dyn fstart_core::services::Console;
}

/// AArch64 UEFI launcher for a memory-mapped FFS and BL31.
#[cfg(all(feature = "crabefi", feature = "aarch64"))]
pub struct Aarch64UefiPayload;

#[cfg(all(feature = "crabefi", feature = "aarch64"))]
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
        fstart_arch::aarch64::boot_bl31_and_resume(config.firmware_addr, boot.fdt_addr());

        let reserved_flash = [MemoryRegion {
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
                ecam_base: Some(config.ecam_base),
                runtime_region: None,
            },
            &reserved_flash,
            config.ram_base,
            config.ram_size,
            config.firmware_data_addr,
            config.firmware_stack_size,
            fdt_reservation,
        )
    }
}

/// Payload launcher selected by fbuild features.
#[cfg(all(feature = "crabefi", feature = "aarch64"))]
pub type BuildSelectedPayload = Aarch64UefiPayload;

/// Payload launcher selected by fbuild features.
#[cfg(all(feature = "crabefi", feature = "x86_64"))]
pub type BuildSelectedPayload = X86UefiPayload;

/// Payload launcher selected by fbuild features.
#[cfg(all(not(feature = "crabefi"), feature = "linux"))]
pub type BuildSelectedPayload = LinuxPayload;

/// Payload launcher used when fbuild selected no payload backend.
#[cfg(all(not(feature = "crabefi"), not(feature = "linux")))]
pub type BuildSelectedPayload = HaltPayload;

/// Device context needed by the common x86 CrabEFI launcher.
pub trait X86UefiPayloadContext {
    /// Return the payload console, if one is available.
    fn console(&self) -> Option<&dyn fstart_core::services::Console>;
    /// Return the detected x86 memory map.
    fn e820(&self) -> &[E820Entry];
    /// Return the firmware image region to reserve for runtime services.
    fn firmware_region(&self) -> (u64, u64);
    /// Return the ACPI RSDP physical address, if ACPI was emitted.
    fn acpi_rsdp(&self) -> Option<u64>;
    /// Return the SMBIOS entry point physical address, if SMBIOS was emitted.
    fn smbios(&self) -> Option<u64> {
        None
    }
    /// Return the PCI ECAM base, if the platform has one.
    fn ecam_base(&self) -> Option<u64>;
}

/// Common x86 CrabEFI payload launcher.
#[cfg(all(feature = "crabefi", feature = "x86_64"))]
pub struct X86UefiPayload;

#[cfg(all(feature = "crabefi", feature = "x86_64"))]
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
                region_type: MemoryType::RuntimeServicesCode,
            },
            MemoryRegion {
                base: acpi_base,
                size: 0x10000,
                region_type: MemoryType::AcpiReclaimable,
            },
        ];

        crate::crabefi::launch_x86_uefi(
            UefiLaunchConfig {
                console: devices.console(),
                framebuffer: None,
                acpi_rsdp: devices.acpi_rsdp(),
                smbios: devices.smbios(),
                fdt: None,
                ecam_base: devices.ecam_base(),
                runtime_region: Some(crate::crabefi::compute_runtime_region()),
            },
            devices.e820(),
            &platform_entries,
        )
    }
}
