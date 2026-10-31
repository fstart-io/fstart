// SPDX-License-Identifier: MIT OR Apache-2.0
//! Boot media and payload handoff: the FFS in the boot flash, verified with
//! the policy the image builder patched into this stage.
use super::layout::Region;
use fstart_core::services::{
    Console, ServiceError,
    memory_detect::{E820Entry, E820Kind, E820State},
};
use fstart_pci::PciRootInfo;
use fstart_stage::boot::{MemoryPolicy, MemoryWindow};
use heapless::Vec;

/// Mount and authenticate the FFS at `ffs`; loads may only go to RAM outside
/// the firmware's own windows.
pub fn mount(e820: &E820State, ffs: Region) -> Result<(), ServiceError> {
    fstart_arch::x86::boot::enable_boot_media_rom_cache();
    let mut writable = Vec::<MemoryWindow, 32>::new();
    let mut reserved = Vec::<MemoryWindow, 32>::new();
    for entry in e820.entries() {
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
    for window in fstart_stage::boot::running_stage_windows()
        .map_err(|_| ServiceError::InvalidParam)?
        .into_iter()
        .chain([MemoryWindow {
            start: ffs.base,
            size: ffs.size,
        }])
    {
        reserved
            .push(window)
            .map_err(|_| ServiceError::InvalidParam)?;
    }
    let policy = MemoryPolicy {
        writable: &writable,
        reserved: &reserved,
        entry_alignment: 1,
    };
    // SAFETY: discovered RAM only, with the live stage, the firmware region
    // and the media excluded; no device has DMA buffers outside the reserved
    // early region.
    unsafe { fstart_stage::directory::set_load_policy(&policy) }
        .map_err(|_| ServiceError::InvalidParam)?;
    let locator = fstart_stage::anchor::media_locator().ok_or(ServiceError::InvalidParam)?;
    if locator.image_offset != 0 || !locator.validate(ffs.size) {
        return Err(ServiceError::InvalidParam);
    }
    let media = fstart_stage::fixed_helpers::MemoryMappedFfs::new(
        ffs.base,
        usize::try_from(locator.image_size).map_err(|_| ServiceError::InvalidParam)?,
    );
    media.mount()?;
    media.verify()
}

/// What the payload launchers need.
pub struct Handoff {
    pub console: &'static dyn Console,
    pub e820: E820State,
    pub rsdp: Option<u64>,
    pub smbios: Option<u64>,
    /// The FFS, the firmware region of the payload.
    pub ffs: Region,
    pub pci_root: PciRootInfo,
    pub devices: heapless::Vec<fstart_core::acpi::AcpiGenericDevice, 16>,
    pub framebuffer: Option<fstart_core::services::FramebufferInfo>,
    #[cfg(feature = "payload-uefi-basic")]
    pub peripherals: super::Peripherals,
}

impl fstart_stage::payload::X86UefiPayloadContext for Handoff {
    fn console(&self) -> Option<&dyn Console> {
        Some(self.console)
    }
    fn e820(&self) -> &[E820Entry] {
        self.e820.entries()
    }
    fn firmware_region(&self) -> (u64, u64) {
        (self.ffs.base, self.ffs.size)
    }
    fn acpi_rsdp(&self) -> Option<u64> {
        self.rsdp
    }
    fn smbios(&self) -> Option<u64> {
        self.smbios
    }
    fn pci_root(&self) -> Option<PciRootInfo> {
        Some(self.pci_root)
    }
    fn acpi_devices(&self) -> &[fstart_core::acpi::AcpiGenericDevice] {
        &self.devices
    }
    #[cfg(feature = "payload-uefi-basic")]
    fn framebuffer(&self) -> Option<fstart_core::services::FramebufferInfo> {
        self.framebuffer
    }
    #[cfg(feature = "payload-uefi-basic")]
    fn take_peripherals(&mut self) -> super::Peripherals {
        core::mem::take(&mut self.peripherals)
    }
}

#[cfg(feature = "payload-linux")]
impl fstart_stage::payload::X86LinuxPayloadContext for Handoff {
    fn e820(&self) -> &[E820Entry] {
        self.e820.entries()
    }
    fn acpi_rsdp(&self) -> Option<u64> {
        self.rsdp
    }
}

/// Identity map the memory map and assigned PCI BARs before the payload.
pub fn map_ram(e820: &E820State, pci: &fstart_pci::PciEcam) -> Result<(), ServiceError> {
    let spans = e820
        .entries()
        .iter()
        .map(|entry| (entry.addr, entry.size))
        .chain(
            pci.assigned_memory_windows()
                .map(|window| (window.base, window.size)),
        );
    // SAFETY: BSP only, before the payload; the tables come from the heap in
    // the reserved early region.
    let tables = unsafe { fstart_arch::x86::boot::paging::IdentityTables::build(spans)? };
    let base = unsafe { tables.install() };
    fstart_log::info!("paging: identity tables at {}", fstart_log::Hex(base));
    Ok(())
}
