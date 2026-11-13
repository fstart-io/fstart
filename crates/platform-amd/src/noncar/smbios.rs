// SPDX-License-Identifier: MIT OR Apache-2.0
//! The SMBIOS tables of the die SoCs ([`crate::smbios`]): one socket, the
//! DRAM ranges of the APOB's memory map, built into reserved firmware
//! memory.
extern crate alloc;

use super::{Ctx, DieSoc};
use alloc::vec::Vec;
use fstart_driver_amd::apob::{
    smbios::{DramRange, dram_ranges},
    sysmap,
};

/// Build the tables; returns their entry point's address.
pub fn install<S: DieSoc>(
    identity: &fstart_acpi::smbios::SmbiosIdentity<'_>,
    ctx: &Ctx<'_, S>,
    cpus: &crate::mp::Cpus,
) -> Option<u64> {
    #[repr(C, align(4096))]
    struct Tables([u8; fstart_acpi::smbios::BUFFER_SIZE]);
    static mut TABLES: Tables = Tables([0; fstart_acpi::smbios::BUFFER_SIZE]);

    let apob = ctx.memory.apob;
    let regions: Vec<DramRange> = apob
        .and_then(|apob| sysmap::first_system_memory_map(apob).ok()?.sorted().ok())
        .and_then(|map| {
            sysmap::memory_map(S::psp(ctx.hw), map.top, map.holes().iter().copied()).ok()
        })
        .map(|regions| dram_ranges(&regions))
        .unwrap_or_default();
    let threads_per_core = fstart_driver_amd::ccx::Thread::current().threads_per_core();
    // SAFETY: firmware BSS in the reserved early region, written once.
    let storage = unsafe { &mut (*core::ptr::addr_of_mut!(TABLES)).0 };
    let address = storage.as_ptr() as u64;
    crate::smbios::build_into(
        storage,
        &crate::smbios::System {
            soc: &S::SMBIOS,
            identity,
            rom_size: S::LAYOUT.flash.size as u32,
            apob,
            regions: &regions,
            cpus,
        },
        // One socket; the SMT threads of a core are adjacent APIC IDs.
        |apic_id| (0, apic_id / u32::from(threads_per_core)),
        |socket, _| socket == 0,
    )
    .inspect_err(|what| fstart_log::error!("smbios: {}", *what))
    .ok()
    .map(|_| address)
}
