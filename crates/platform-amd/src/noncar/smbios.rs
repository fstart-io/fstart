// SPDX-License-Identifier: MIT OR Apache-2.0
//! The SMBIOS tables of the flow ([`fstart_driver_amd::apob::smbios`]):
//! the board's identity, one processor package, the DIMM positions of the
//! APOB's SMBIOS seeds with their SPD copies, and the APOB's DRAM ranges.
extern crate alloc;

use super::{Board, Soc};
use alloc::vec::Vec;
use fstart_arch::x86::boot::cpuid::ProcessorSnapshot;
use fstart_driver_amd::apob::{
    Apob, mem,
    smbios::{DramRange, Inputs, Processor, build_into, dimms, dram_ranges},
    sysmap::{self, MemoryMap},
};
use fstart_log::Hex;

/// Build the tables into reserved firmware memory; returns their entry
/// point's address.
pub fn install<S: Soc, B: Board<Soc = S>>(
    apob: Option<&Apob>,
    map: &MemoryMap,
    apic_ids: &[u32],
) -> Option<u64> {
    #[repr(C, align(4096))]
    struct Tables([u8; fstart_acpi::smbios::BUFFER_SIZE]);
    static mut TABLES: Tables = Tables([0; fstart_acpi::smbios::BUFFER_SIZE]);

    let thread = fstart_driver_amd::ccx::Thread::current();
    let cores = apic_ids
        .iter()
        .filter(|&&id| thread.with_apic_id(id as u8).core_primary())
        .count() as u16;
    let processors = [Processor {
        socket: 0,
        snapshot: ProcessorSnapshot::read(),
        apic_ids,
        enabled_cores: cores,
    }];
    let dimms = apob
        .map(|apob| dimms(apob, S::SMBIOS.channels_per_die, |socket, _| socket == 0))
        .unwrap_or_default();
    let regions: Vec<DramRange> = sysmap::memory_map(S::PSP, map.top, map.holes().iter().copied())
        .map(|regions| dram_ranges(&regions))
        .unwrap_or_default();
    let ecc = apob
        .and_then(|apob| mem::smbios_info(apob).ok())
        .is_some_and(|seeds| seeds.ecc());
    // SAFETY: firmware BSS in the reserved early region, written once.
    let storage = unsafe { &mut (*core::ptr::addr_of_mut!(TABLES)).0 };
    let address = storage.as_ptr() as u64;
    let used = build_into(
        storage,
        address,
        &Inputs {
            identity: B::smbios_identity(),
            processors: &processors,
            dimms: &dimms,
            regions: &regions,
            max_dimm_mib: S::SMBIOS.max_dimm_mib,
            socket_upgrade: S::SMBIOS.socket_upgrade,
            ecc,
            rom_size: S::LAYOUT.flash.size as u32,
            uefi: cfg!(fstart_payload = "crabefi"),
        },
    )
    .ok()?;
    // For Linux without UEFI.
    // SAFETY: the fixed MTRRs route C0000-FFFFF to DRAM; the e820 reserves
    // it for the firmware.
    unsafe { fstart_acpi::smbios::publish_legacy_entry_point(storage) };
    fstart_log::info!(
        "smbios: {} DIMM positions, {} installed, {} bytes at {}",
        dimms.len() as u32,
        dimms.iter().filter(|d| d.device.is_populated()).count() as u32,
        used as u32,
        Hex(address)
    );
    Some(address)
}
