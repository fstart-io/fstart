// SPDX-License-Identifier: MIT OR Apache-2.0
//! The SMBIOS inventory of an AMD system over the shared SMBIOS writer and
//! architectural CPU snapshots: one processor per package with its caches,
//! the DIMM positions the ABL reports and the DRAM ranges. Board identity
//! is supplied separately; CPU/DIMM inventory is runtime data. SP3 and the
//! noncar SoCs fill [`Inputs`] from their APOB.
extern crate alloc;

use alloc::format;
use fstart_acpi::smbios::{self, ProcessorInfo, SmbiosIdentity, WriteError};
use fstart_arch::x86::boot::cpuid::ProcessorSnapshot;
use fstart_core::{memory_info::MemoryDevice, smbios::CacheAssociativity};

/// A DRAM range of the memory map, for type 19.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DramRange {
    pub base: u64,
    /// Exclusive.
    pub end: u64,
}

/// One homogeneous CPU package. APIC IDs include only enabled threads;
/// cache geometry is read on a real member of this package, not copied from BSP.
pub struct Processor<'a> {
    pub socket: u8,
    pub snapshot: ProcessorSnapshot,
    pub apic_ids: &'a [u32],
    pub enabled_cores: u16,
}

pub struct Dimm {
    pub socket: u8,
    pub die: u8,
    pub device: MemoryDevice,
}

pub struct Inputs<'a> {
    pub identity: SmbiosIdentity<'a>,
    pub processors: &'a [Processor<'a>],
    pub dimms: &'a [Dimm],
    /// The ABL's DRAM ranges before firmware allocations, not `0..total_RAM`.
    pub regions: &'a [DramRange],
    /// Supported maximum per physical DIMM position, not installed capacity.
    pub max_dimm_mib: u32,
    /// SMBIOS processor upgrade (socket) code: 0x37 SP3, 0x06 none for the
    /// BGA packages.
    pub socket_upgrade: u8,
    /// APOB 8/8's active ECC mode; SPD ECC width alone proves no active mode.
    pub ecc: bool,
    pub rom_size: u32,
    pub uefi: bool,
}

/// Pure, bounded table construction, also usable by host tests.
pub fn build_into(bytes: &mut [u8], address: u64, input: &Inputs<'_>) -> Result<usize, WriteError> {
    smbios::assemble_into(bytes, address, |writer| {
        writer.add_identity(&input.identity, input.rom_size, input.uefi);
        for processor in input.processors {
            let cpu = &processor.snapshot;
            let mut caches = [0xffff; 3];
            for cache in &cpu.caches[..cpu.cache_count.min(cpu.caches.len())] {
                if !(1..=3).contains(&cache.level) {
                    continue;
                }
                let instances = cache.instance_count(processor.apic_ids);
                let size_kib = u64::from(cache.size_bytes) / 1024 * instances as u64;
                let Ok(size_kib) = u32::try_from(size_kib) else {
                    continue;
                };
                if size_kib == 0 {
                    continue;
                }
                let kind = match cache.cache_type {
                    1 => 0x04,
                    2 => 0x03,
                    3 => 0x05,
                    _ => 0x02,
                };
                let label = format!(
                    "Socket{} L{} {}",
                    processor.socket,
                    cache.level,
                    match cache.cache_type {
                        1 => "Data",
                        2 => "Instruction",
                        _ => "Unified",
                    }
                );
                caches[usize::from(cache.level - 1)] = writer.add_cache_info(
                    &label,
                    cache.level,
                    size_kib,
                    CacheAssociativity::from_ways(cache.associativity, cache.fully_associative)
                        .to_smbios_byte(),
                    kind,
                );
            }
            let logical_label = format!("Socket{}", processor.socket);
            let socket = input
                .identity
                .processor_sockets
                .get(usize::from(processor.socket))
                .copied()
                .unwrap_or(&logical_label);
            let threads = u16::try_from(processor.apic_ids.len()).unwrap_or(0);
            let characteristics = [
                (cpu.extended_features & (1 << 29) != 0, 1 << 2), // long mode
                (processor.enabled_cores > 1, 1 << 3),
                (threads > processor.enabled_cores, 1 << 4),
                (cpu.extended_features & (1 << 20) != 0, 1 << 5), // NX
                (cpu.extended_features_ecx & (1 << 2) != 0, 1 << 6), // SVM
            ]
            .into_iter()
            .filter(|(yes, _)| *yes)
            .fold(0, |bits, (_, bit)| bits | bit);
            writer.add_processor(&ProcessorInfo {
                socket,
                manufacturer: cpu.manufacturer(),
                version: cpu.brand(),
                family: 0x6b, // AMD Zen: Zen 1, 2 and 3
                id: u64::from(cpu.signature) | (u64::from(cpu.standard_features) << 32),
                // No guessed BCLK, boost clock, or instantaneous frequency.
                external_clock_mhz: 0,
                max_speed_mhz: 0,
                current_speed_mhz: 0,
                upgrade: input.socket_upgrade,
                core_count: cpu.core_count.max(processor.enabled_cores),
                core_enabled: processor.enabled_cores,
                thread_count: threads,
                characteristics,
                caches,
            });
        }
        if !input.dimms.is_empty() && input.max_dimm_mib != 0 {
            writer.add_physical_memory_array(
                u64::from(input.max_dimm_mib) * 1024 * input.dimms.len() as u64,
                input.dimms.len() as u16,
                if input.ecc { 6 } else { 3 },
            );
            for dimm in input.dimms {
                // These are explicit topology labels, not unverified silkscreen
                // names or DIMM-to-error/SEL mappings.
                let bank = format!(
                    "Socket{} Die{} Channel{}",
                    dimm.socket, dimm.die, dimm.device.channel
                );
                let locator = format!("{} DIMM{}", bank, dimm.device.slot);
                writer.add_dimm(&dimm.device, &locator, &bank);
            }
            for region in input.regions.iter().filter(|r| r.end > r.base) {
                // Channel/rank interleave width is not known from an address
                // range. SMBIOS uses zero for an unknown partition width.
                writer.add_memory_array_mapped_address(region.base, region.end - 1, 0);
            }
        }
        writer.add_system_boot_info();
        writer.add_end_of_table();
    })
}

#[cfg(test)]
mod tests;
