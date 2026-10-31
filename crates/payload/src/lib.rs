//! Payload launchers: Linux, UEFI (CrabEFI) and coreboot payloads.
//!
//! The platform describes the machine once ([`Description`]) and adds where
//! the launcher places the payload's pieces ([`Placement`]); the
//! build-selected launcher reads the [`Handoff`] and exports what its
//! payload needs (e820 and zero page, coreboot tables, device tree, UEFI).

#![no_std]

extern crate alloc as heap;

// The launchers load through the stage crate's verified loaders; a build
// with only the halt launcher calls none of them.
use fstart_stage as _;

pub use fstart_core::description::{AcpiTables, Description, PciRootInfo, Range};

#[cfg(feature = "crabefi-basic")]
pub use fstart_boot::crabefi;
#[cfg(all(feature = "coreboot", any(target_arch = "x86", target_arch = "x86_64")))]
pub mod coreboot;

/// Build-selected payload launcher. `devices` carries what a platform's own
/// launcher needs beyond the handoff (its boot media); the common
/// launchers take any.
pub trait MainstagePayload<D = ()> {
    /// Hand the machine to the payload.
    fn boot(handoff: Handoff<'_>, devices: D) -> !;
}

/// Where the launcher places the payload's pieces, from the platform layout.
#[derive(Clone, Copy, Default)]
pub struct Placement {
    /// Linux kernel load address (Arm, RISC-V).
    pub kernel: u64,
    /// Where the payload's device tree is prepared (Arm, RISC-V).
    pub device_tree: u64,
    /// SBI or BL31 firmware: its load address and the RAM it keeps.
    pub firmware: Range,
    /// Boot hart (RISC-V).
    pub hart_id: u64,
    pub bootargs: &'static str,
    /// RAM for coreboot payload files and tables (x86).
    #[cfg(feature = "coreboot")]
    pub coreboot_window: Option<crate::coreboot::Window>,
}

/// What the platform hands the payload launcher.
pub struct Handoff<'a> {
    pub description: Description<'a>,
    pub placement: Placement,
    /// Devices on platform-owned buses for CrabEFI's drivers.
    #[cfg(feature = "crabefi-basic")]
    pub peripherals: heap::vec::Vec<crate::crabefi::peripheral::Peripheral>,
}

impl<'a> Handoff<'a> {
    #[must_use]
    pub fn new(description: Description<'a>, placement: Placement) -> Self {
        Self {
            description,
            placement,
            #[cfg(feature = "crabefi-basic")]
            peripherals: heap::vec::Vec::new(),
        }
    }
}

/// Payload launcher used when the build selects no payload.
pub struct HaltPayload;

impl<D> MainstagePayload<D> for HaltPayload {
    fn boot(_: Handoff<'_>, _: D) -> ! {
        loop {
            core::hint::spin_loop();
        }
    }
}

/// Common Linux payload launcher.
#[cfg(feature = "linux")]
pub struct LinuxPayload;

#[cfg(feature = "linux")]
impl<D> MainstagePayload<D> for LinuxPayload {
    fn boot(handoff: Handoff<'_>, _: D) -> ! {
        use fstart_stage::fixed_helpers::MemoryMappedLinuxBoot;

        let (description, placement) = (handoff.description, handoff.placement);
        let Some(ram) = description.ram() else {
            fstart_log::error!("Linux payload: no RAM described");
            halt_linux();
        };
        let mut boot = MemoryMappedLinuxBoot::new(
            description.firmware.base,
            description.firmware.size as usize,
            description.device_tree.unwrap_or(0),
        );
        if boot.mount().is_err()
            || boot.verify().is_err()
            || if placement.firmware.base == 0 {
                boot.load_kernel().is_err()
            } else {
                boot.load_firmware_and_kernel().is_err()
            }
            // Without a device tree from the boot ROM or emulator, the FFS
            // carries the board's.
            || (description.device_tree.is_none() && boot.load_fdt(placement.device_tree).is_err())
            || boot
                .prepare_fdt(placement.device_tree, placement.bootargs, ram.base, ram.size)
                .is_err()
        {
            fstart_log::error!("Linux payload: FFS payload load failed");
            halt_linux();
        }

        let Ok(params) = boot.checked_boot_params(
            placement.kernel,
            placement.firmware.base,
            placement.hart_id,
            placement.bootargs,
        ) else {
            fstart_log::error!("Linux payload: configured entry was not verified");
            halt_linux();
        };
        launch_linux(&params)
    }
}

#[cfg(feature = "linux")]
fn halt_linux() -> ! {
    #[cfg(target_arch = "riscv64")]
    fstart_arch::riscv64::halt();
    #[cfg(target_arch = "aarch64")]
    fstart_arch::aarch64::halt();
    #[cfg(target_arch = "arm")]
    fstart_arch::halt();
    #[cfg(not(any(target_arch = "riscv64", target_arch = "aarch64", target_arch = "arm")))]
    loop {
        core::hint::spin_loop();
    }
}

#[cfg(feature = "linux")]
fn launch_linux(params: &fstart_core::services::BootLinuxParams<'_>) -> ! {
    #[cfg(target_arch = "riscv64")]
    fstart_arch::riscv64::boot_linux(params);
    #[cfg(target_arch = "aarch64")]
    fstart_arch::aarch64::boot_linux(params);
    #[cfg(target_arch = "arm")]
    fstart_arch::armv7::boot_linux(params);
    #[cfg(not(any(target_arch = "riscv64", target_arch = "aarch64", target_arch = "arm")))]
    {
        let _ = params;
        halt_linux()
    }
}

/// AArch64 UEFI launcher for a memory-mapped FFS and BL31.
#[cfg(all(feature = "crabefi-basic", target_arch = "aarch64"))]
pub struct Aarch64UefiPayload;

#[cfg(all(feature = "crabefi-basic", target_arch = "aarch64"))]
impl<D> MainstagePayload<D> for Aarch64UefiPayload {
    fn boot(handoff: Handoff<'_>, _: D) -> ! {
        use crate::crabefi::UefiLaunchConfig;
        use fstart_stage::fixed_helpers::MemoryMappedUefiBoot;

        let (description, placement) = (handoff.description, handoff.placement);
        let Some(ram) = description.ram() else {
            fstart_log::error!("AArch64 UEFI payload: no RAM described");
            fstart_arch::aarch64::halt();
        };
        let mut boot = MemoryMappedUefiBoot::new(
            description.firmware.base,
            description.firmware.size as usize,
            description.device_tree.unwrap_or(0),
        );
        if boot.mount().is_err() || boot.verify().is_err() || boot.load_firmware().is_err() {
            fstart_log::error!("AArch64 UEFI payload: BL31 load failed");
            fstart_arch::aarch64::halt();
        }

        // SAFETY: QEMU supplied this FDT and it remains valid through CrabEFI.
        let fdt = unsafe { boot.fdt_bytes() };
        // SAFETY: QEMU supplied the FDT header at the same stable address.
        let fdt_reservation = unsafe { boot.fdt_reservation() };
        let Ok(entry) = boot.checked_firmware_entry(placement.firmware.base) else {
            fstart_log::error!("AArch64 UEFI payload: configured BL31 entry was not verified");
            fstart_arch::aarch64::halt();
        };
        fstart_arch::aarch64::boot_bl31_and_resume(entry, boot.fdt_addr());
        fstart_log::info!("aarch64 uefi: BL31 returned, launching CrabEFI");

        let ecam = uefi_ecam(description.pci_root);
        let static_entries = static_regions(&description);
        crate::crabefi::launch_flat_uefi(
            UefiLaunchConfig {
                acpi_devices: &[],
                console: description.console,
                peripherals: handoff.peripherals,
                framebuffer: None,
                acpi_rsdp: None,
                smbios: None,
                fdt,
                ecam_regions: ecam.as_slice(),
            },
            &static_entries,
            ram.base,
            ram.size,
            fdt_reservation,
        )
    }
}

/// The description's reserved and MMIO ranges as UEFI memory regions, and
/// `extra` ahead of them.
#[cfg(all(
    feature = "crabefi-basic",
    any(target_arch = "aarch64", target_arch = "riscv64")
))]
fn static_regions(description: &Description<'_>) -> heap::vec::Vec<crate::crabefi::MemoryRegion> {
    use crate::crabefi::{MemoryRegion, MemoryType};
    let region = |range: &Range, region_type| MemoryRegion {
        base: range.base,
        size: range.size,
        region_type,
    };
    description
        .reserved
        .iter()
        .map(|range| region(range, MemoryType::Reserved))
        .chain(
            description
                .mmio
                .iter()
                .map(|range| region(range, MemoryType::Mmio)),
        )
        .collect()
}

/// RISC-V UEFI launcher using OpenSBI for the S-mode transition.
#[cfg(all(feature = "crabefi-basic", target_arch = "riscv64"))]
pub struct Riscv64UefiPayload;

#[cfg(all(feature = "crabefi-basic", target_arch = "riscv64"))]
impl Riscv64UefiPayload {
    /// Load OpenSBI, then ask it to resume the board-selected S-mode entry.
    pub fn boot(handoff: Handoff<'_>) -> ! {
        use fstart_stage::fixed_helpers::MemoryMappedUefiBoot;

        let (description, placement) = (handoff.description, handoff.placement);
        let dtb_addr = fstart_arch::riscv64::boot_dtb_addr();
        let mut boot = MemoryMappedUefiBoot::new(
            description.firmware.base,
            description.firmware.size as usize,
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
        let Ok(entry) = boot.checked_firmware_entry(placement.firmware.base) else {
            fstart_log::error!("RISC-V UEFI payload: configured OpenSBI entry was not verified");
            fstart_arch::riscv64::halt();
        };
        // OpenSBI adds DTB properties/reservations. Give it the dedicated
        // writable workspace, not QEMU's exactly bounded source blob. On
        // return, copy_fdt_to_workspace accepts growth only within that same
        // registered capacity; do not relax the original source bound.
        let Ok(used) = fstart_stage::copy_fdt_to_workspace(boot.fdt_addr(), placement.device_tree)
        else {
            fstart_log::error!("RISC-V UEFI payload: bounded FDT preparation failed");
            fstart_arch::riscv64::halt();
        };
        let capacity = fstart_stage::fdt_workspace_capacity();
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
            placement.device_tree,
            &info,
        )
    }

    /// Enter CrabEFI after OpenSBI has switched the processor to S-mode.
    pub fn resume_sbi(handoff: Handoff<'_>, hart_id: u64, dtb_addr: u64) -> ! {
        use crate::crabefi::{MemoryRegion, MemoryType, UefiLaunchConfig};

        let (description, placement) = (handoff.description, handoff.placement);
        let Some(ram) = description.ram() else {
            fstart_log::error!("RISC-V UEFI payload: no RAM described");
            fstart_arch::riscv64::halt();
        };
        let Ok(fdt_size) = fstart_stage::copy_fdt_to_workspace(dtb_addr, placement.device_tree)
        else {
            fstart_log::error!("RISC-V UEFI payload: bounded FDT copy failed");
            fstart_arch::riscv64::halt();
        };
        // SAFETY: bounded copy validated and initialized the dedicated workspace;
        // this flow makes no further mutations before passing it to CrabEFI.
        let fdt = Some(unsafe {
            core::slice::from_raw_parts(placement.device_tree as *const u8, fdt_size)
        });
        let fdt_reservation =
            fdt.map(|bytes| (placement.device_tree, (bytes.len() as u64 + 0xfff) & !0xfff));
        crate::crabefi::set_riscv_boot_hartid(hart_id);

        let ecam = uefi_ecam(description.pci_root);
        // OpenSBI stays resident in the RAM it was loaded to.
        let mut static_entries = static_regions(&description);
        static_entries.push(MemoryRegion {
            base: placement.firmware.base,
            size: placement.firmware.size,
            region_type: MemoryType::Reserved,
        });
        crate::crabefi::launch_flat_uefi(
            UefiLaunchConfig {
                acpi_devices: &[],
                console: description.console,
                peripherals: handoff.peripherals,
                framebuffer: None,
                acpi_rsdp: None,
                smbios: None,
                fdt,
                ecam_regions: ecam.as_slice(),
            },
            &static_entries,
            ram.base,
            ram.size,
            fdt_reservation,
        )
    }
}

#[cfg(all(feature = "crabefi-basic", target_arch = "riscv64"))]
impl<D> MainstagePayload<D> for Riscv64UefiPayload {
    fn boot(handoff: Handoff<'_>, _: D) -> ! {
        Self::boot(handoff)
    }
}

/// Direct x86 Linux payload launcher.
///
/// x86 has no device tree: the launcher loads the bzImage payload, hands the
/// kernel the description's memory map, ACPI RSDP and command line through
/// the zero page, and jumps to the kernel's 64-bit entry point. Launch policy
/// (load address, zero page, bootargs) arrives via the verified FFS manifest.
#[cfg(all(feature = "linux", any(target_arch = "x86", target_arch = "x86_64")))]
pub struct X86LinuxPayload;

#[cfg(all(feature = "linux", any(target_arch = "x86", target_arch = "x86_64")))]
impl<D> MainstagePayload<D> for X86LinuxPayload {
    fn boot(handoff: Handoff<'_>, _: D) -> ! {
        let description = handoff.description;
        use fstart_stage::fixed_helpers::MemoryMappedLinuxBoot;

        let Some(manifest_bytes) = fstart_core::services::ffs_context::read_verified_asset(
            fstart_core::payload_manifest::X86_LINUX_MANIFEST_ASSET,
        ) else {
            halt_x86_linux("no verified x86 Linux payload manifest");
        };
        let Some(config) = fstart_core::payload_manifest::X86LinuxManifest::decode(manifest_bytes)
        else {
            halt_x86_linux("invalid x86 Linux payload manifest");
        };
        // Load through the window the mainstage already mounted and verified:
        // the FFS container is smaller than the flash it is mapped into, so its
        // extent comes from that published context, never from the flash size.
        let Some(context) = fstart_core::services::ffs_context::memory_mapped() else {
            halt_x86_linux("no mounted boot media");
        };
        let Ok(media_size) = usize::try_from(context.image_size) else {
            halt_x86_linux("boot media window is too large");
        };
        fstart_log::info!(
            "x86 Linux payload: boot media at {:#x} ({} bytes)",
            context.image_base,
            context.image_size
        );
        let mut boot = MemoryMappedLinuxBoot::new(context.image_base, media_size, 0);
        if boot.mount().is_err() {
            halt_x86_linux("boot media mount failed");
        }
        if boot.verify().is_err() {
            halt_x86_linux("boot media verification failed");
        }
        if boot.load_kernel().is_err() {
            halt_x86_linux("kernel payload load failed");
        }
        fstart_log::info!(
            "x86 Linux payload: kernel loaded at {:#x}",
            boot.kernel_addr()
        );
        // An absent RSDP is passed as zero: the kernel then finds no ACPI
        // tables, which is better than handing it a wrong address.
        let rsdp = description.acpi.map_or(0, |acpi| acpi.rsdp);
        let Ok(params) = boot.x86_boot_params(
            config.kernel_load_addr,
            config.zero_page_addr,
            rsdp,
            description.memory,
            config.bootargs,
            config.print_x86_mtrrs,
        ) else {
            halt_x86_linux("configured entry was not verified");
        };
        #[cfg(feature = "tcg")]
        measure_x86_linux(config.bootargs);
        fstart_stage::timestamps::handoff();
        fstart_arch::x86::boot::boot_linux(&params)
    }
}

/// fstart boots Linux itself, so it ends the pre-OS phase (separators,
/// TPM locked) and then measures the boot target like a boot manager: the
/// kernel into PCR 4, and the command line as Linux's EFI stub measures its
/// load options, a tagged event in PCR 9 over the UCS-2 string with its
/// terminating NUL, so a verifier recomputes it the same way for both.
#[cfg(all(
    feature = "tcg",
    feature = "linux",
    any(target_arch = "x86", target_arch = "x86_64")
))]
fn measure_x86_linux(bootargs: &str) {
    use fstart_stage::tcg::event::{EV_EFI_PLATFORM_FIRMWARE_BLOB2, EV_EVENT_TAG, EventData};
    /// `LOAD_OPTIONS_EVENT_TAG_ID` of the Linux EFI stub.
    const LOAD_OPTIONS_TAG: u32 = 0x8F3B_22ED;
    fstart_stage::tcg::handoff(fstart_stage::tcg::Handoff::Final);
    if !fstart_stage::tcg::measure_file_by_type(
        fstart_core::ffs::FileType::Payload,
        4,
        EV_EFI_PLATFORM_FIRMWARE_BLOB2,
        None,
    ) {
        fstart_log::error!("x86 Linux payload: kernel not measured");
        fstart_tcg::measurement_error();
    }
    let load_options: heap::vec::Vec<u8> = bootargs
        .encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    fstart_tcg::measure(
        9,
        EV_EVENT_TAG,
        &load_options,
        EventData::tagged(LOAD_OPTIONS_TAG, b"LOADED_IMAGE::LoadOptions\0").as_bytes(),
    );
    fstart_stage::tcg::check();
}

#[cfg(all(feature = "linux", any(target_arch = "x86", target_arch = "x86_64")))]
fn halt_x86_linux(reason: &str) -> ! {
    fstart_log::error!("x86 Linux payload: {}", reason);
    fstart_arch::x86::boot::halt()
}

#[cfg(any(
    all(fstart_payload = "halt", fstart_payload = "linux"),
    all(fstart_payload = "halt", fstart_payload = "crabefi"),
    all(fstart_payload = "halt", fstart_payload = "coreboot"),
    all(fstart_payload = "linux", fstart_payload = "crabefi"),
    all(fstart_payload = "linux", fstart_payload = "coreboot"),
    all(fstart_payload = "crabefi", fstart_payload = "coreboot")
))]
compile_error!("select exactly one payload launcher");
#[cfg(all(
    fstart_payload = "coreboot",
    not(all(feature = "coreboot", any(target_arch = "x86", target_arch = "x86_64")))
))]
compile_error!("selected coreboot backend is not enabled");
#[cfg(all(fstart_payload = "linux", not(feature = "linux")))]
compile_error!("selected Linux backend is not enabled");
#[cfg(all(fstart_payload = "crabefi", not(feature = "crabefi-basic")))]
compile_error!("selected CrabEFI backend is not enabled");

/// Build selection, independent of the set of available payload backends.
#[cfg(all(fstart_payload = "crabefi", target_arch = "aarch64"))]
pub type BuildSelectedPayload = Aarch64UefiPayload;
#[cfg(all(fstart_payload = "crabefi", target_arch = "riscv64"))]
pub type BuildSelectedPayload = Riscv64UefiPayload;
#[cfg(all(
    fstart_payload = "crabefi",
    any(target_arch = "x86", target_arch = "x86_64")
))]
pub type BuildSelectedPayload = X86UefiPayload;
#[cfg(all(
    fstart_payload = "linux",
    any(target_arch = "x86", target_arch = "x86_64")
))]
pub type BuildSelectedPayload = X86LinuxPayload;
#[cfg(all(
    fstart_payload = "linux",
    not(any(target_arch = "x86", target_arch = "x86_64"))
))]
pub type BuildSelectedPayload = LinuxPayload;
#[cfg(fstart_payload = "coreboot")]
pub type BuildSelectedPayload = crate::coreboot::X86CorebootPayload;
#[cfg(fstart_payload = "halt")]
pub type BuildSelectedPayload = HaltPayload;

#[cfg(all(
    test,
    any(target_arch = "x86", target_arch = "x86_64"),
    feature = "linux",
    feature = "crabefi-basic",
    any(
        fstart_payload = "halt",
        fstart_payload = "linux",
        fstart_payload = "crabefi"
    )
))]
mod selection_tests {
    use super::*;

    // Run once for each selection with BOTH backends enabled.
    #[test]
    fn selection_is_independent_of_available_backends() {
        #[cfg(fstart_payload = "halt")]
        type Expected = HaltPayload;
        #[cfg(fstart_payload = "linux")]
        type Expected = X86LinuxPayload;
        #[cfg(fstart_payload = "crabefi")]
        type Expected = X86UefiPayload;
        assert_eq!(
            core::any::TypeId::of::<BuildSelectedPayload>(),
            core::any::TypeId::of::<Expected>()
        );
    }
}

/// Common x86 CrabEFI payload launcher.
#[cfg(all(
    feature = "crabefi-basic",
    any(target_arch = "x86", target_arch = "x86_64")
))]
pub struct X86UefiPayload;

#[cfg(all(
    feature = "crabefi-basic",
    any(target_arch = "x86", target_arch = "x86_64")
))]
impl<D> MainstagePayload<D> for X86UefiPayload {
    fn boot(handoff: Handoff<'_>, _: D) -> ! {
        use crate::crabefi::{MemoryRegion, MemoryType, UefiLaunchConfig};

        let description = handoff.description;
        let firmware = description.firmware;
        // Without ACPI tables there is nothing to reserve as NVS: an empty
        // region at the old default keeps the memory map shape.
        let acpi = description
            .acpi
            .map_or(Range::new(0, 0x10000), |acpi| acpi.region);
        let platform_entries = [
            MemoryRegion {
                base: firmware.base,
                size: firmware.size,
                region_type: MemoryType::Reserved,
            },
            MemoryRegion {
                base: acpi.base,
                size: acpi.size,
                region_type: MemoryType::AcpiNvs,
            },
        ];
        // CrabEFI continues the measured boot: fstart locks the TPM, CrabEFI
        // measures the boot and extends the separators.
        #[cfg(feature = "tcg")]
        fstart_stage::tcg::handoff(fstart_stage::tcg::Handoff::Locked);

        let ecam = uefi_ecam(description.pci_root);
        fstart_stage::timestamps::handoff();
        crate::crabefi::launch_x86_uefi(
            UefiLaunchConfig {
                console: description.console,
                acpi_devices: description.acpi_devices,
                peripherals: handoff.peripherals,
                framebuffer: description.framebuffer.map(|info| {
                    crate::crabefi::FramebufferConfig {
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
                    }
                }),
                acpi_rsdp: description.acpi.map(|acpi| acpi.rsdp),
                smbios: description.smbios,
                fdt: None,
                ecam_regions: ecam.as_slice(),
            },
            description.memory,
            &platform_entries,
        )
    }
}

#[cfg(feature = "crabefi-basic")]
fn uefi_ecam(root: Option<PciRootInfo>) -> Option<crate::crabefi::PciEcamRegion> {
    root.map(|root| crate::crabefi::PciEcamRegion {
        base: root.ecam_base,
        segment: root.segment,
        bus_start: root.bus_start,
        bus_end: root.bus_end,
    })
}
