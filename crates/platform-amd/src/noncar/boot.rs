// SPDX-License-Identifier: MIT OR Apache-2.0
//! Boot media and payload handoff: the FFS in the boot flash, verified with
//! the policy the image builder patched into this stage.
use super::layout::Region;
use fstart_core::services::{ServiceError, memory_detect::E820State};
use fstart_stage::boot::MemoryWindow;

/// Mount and authenticate the FFS at `ffs`; loads may only go to RAM outside
/// the firmware's own windows: its `early` reservation (the running image,
/// its page tables, BSS, heap and stack) and the media.
pub fn mount(e820: &E820State, early: Region, ffs: Region) -> Result<(), ServiceError> {
    fstart_arch::x86::boot::enable_boot_media_rom_cache();
    let window = |region: Region| MemoryWindow {
        start: region.base,
        size: region.size,
    };
    crate::media::mount(
        e820.entries(),
        &[window(early)],
        window(ffs),
        const { fstart_stage::boot::codec(crate::compression::PAYLOAD) },
    )
    .map_err(|_| ServiceError::InvalidParam)
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
