// SPDX-License-Identifier: MIT OR Apache-2.0
//! The payload's page tables: the reset entry maps the first 512 GiB only.
use fstart_core::services::{ServiceError, memory_detect::E820State};

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
