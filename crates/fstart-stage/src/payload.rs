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

/// Payload launcher selected by build features.
#[cfg(not(feature = "crabefi"))]
pub type BuildSelectedPayload = HaltPayload;

/// Payload launcher selected by build features.
#[cfg(feature = "crabefi")]
pub type BuildSelectedPayload = X86UefiPayload;

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
#[cfg(feature = "crabefi")]
pub struct X86UefiPayload;

#[cfg(feature = "crabefi")]
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
