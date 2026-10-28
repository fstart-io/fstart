//! SP3 inventory adapter over the shared SMBIOS writer and architectural CPU
//! snapshots. No table framing, SPD decoder, MMIO or ECAM implementation lives
//! here. Board identity is supplied separately; CPU/DIMM inventory is runtime
//! data, not a description of the bring-up bench.

extern crate alloc;

use alloc::format;
use fstart_acpi::smbios::{self, ProcessorInfo, SmbiosIdentity, WriteError};
use fstart_arch::x86::boot::cpuid::ProcessorSnapshot;
use fstart_core::{memory_info::MemoryDevice, smbios::CacheAssociativity};
use fstart_driver_amd::apob::sysmap::Region;

/// One homogeneous SP3 CPU package. APIC IDs include only enabled threads;
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
    /// The ABL memory map before firmware allocations, not `0..total_RAM`.
    pub regions: &'a [Region],
    /// Supported maximum per physical DIMM position, not installed capacity.
    pub max_dimm_mib: u32,
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
                family: 0x6b, // AMD Zen, all three SP3 generations
                id: u64::from(cpu.signature) | (u64::from(cpu.standard_features) << 32),
                // No guessed BCLK, boost clock, or instantaneous frequency.
                external_clock_mhz: 0,
                max_speed_mhz: 0,
                current_speed_mhz: 0,
                upgrade: 0x37, // Socket SP3 (not SP3r2 or a BGA socket)
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
            for region in input
                .regions
                .iter()
                .filter(|region| region.kind.is_ram() && region.size != 0)
            {
                // Channel/rank interleave width is not known from an address
                // range. SMBIOS uses zero for an unknown partition width.
                writer.add_memory_array_mapped_address(region.base, region.end() - 1, 0);
            }
        }
        writer.add_system_boot_info();
        writer.add_end_of_table();
    })
}

#[cfg(feature = "bundle-reset")]
pub(crate) mod runtime {
    use super::*;
    use crate::flow::{Ctx, OrHalt};
    use alloc::vec::Vec;
    use fstart_driver_amd::{
        apob::{mem, sysmap},
        sp3::{ccx::topology::ApicLayout, soc::Generation},
    };

    /// Capture package-local CPUID before APs are parked for the payload and
    /// publish immutable tables in the existing reserved firmware heap.
    /// `rom_size` is the image extent recorded by the flash anchor, not a
    /// possibly partial EFS mapping or a physical flash-geometry measurement.
    pub(crate) fn prepare<B: crate::board::Board>(
        ctx: &Ctx,
        mp: &fstart_arch::x86::mp::MpHandle,
        rom_size: u32,
    ) -> u64 {
        let smt = (fstart_arch::x86::cpuid(0x8000_001e).1 >> 8) & 0xff != 0;
        let layout = ApicLayout::new(ctx.generation, smt);
        let ids: Vec<Vec<u32>> = (0..ctx.topology.sockets)
            .map(|socket| {
                ctx.apic_ids
                    .iter()
                    .copied()
                    .filter(|id| layout.decode(*id).socket == socket)
                    .collect()
            })
            .collect();
        let mut processors = Vec::new();
        for (socket, apic_ids) in ids.iter().enumerate() {
            let Some(&leader) = apic_ids.first() else {
                continue;
            };
            let index = ctx
                .apic_ids
                .iter()
                .position(|&id| id == leader)
                .or_halt("SMBIOS CPU leader");
            let snapshot = if index == 0 {
                ProcessorSnapshot::read()
            } else {
                // Existing bounded MPMC queue transfers an owned value from a
                // selected AP; no custom unsafe shared result slot or locks.
                let queue = heapless::mpmc::MpMcQueue::<ProcessorSnapshot, 2>::new();
                mp.scope(|scope| {
                    scope.run_on(index as u16, &|| {
                        let _ = queue.enqueue(ProcessorSnapshot::read());
                    })
                });
                queue.dequeue().or_halt("SMBIOS package CPUID")
            };
            let cores = apic_ids
                .iter()
                .enumerate()
                .filter(|(i, id)| {
                    let core = **id >> u32::from(smt);
                    !apic_ids[..*i]
                        .iter()
                        .any(|prior| *prior >> u32::from(smt) == core)
                })
                .count() as u16;
            processors.push(Processor {
                socket: socket as u8,
                snapshot,
                apic_ids,
                enabled_cores: cores,
            });
        }
        let seeds = mem::smbios_info(&ctx.apob).ok();
        if seeds.is_none() {
            fstart_log::warn!("smbios: memory seeds unavailable; omitting memory inventory");
        }
        let mut dimms = Vec::new();
        for seed in seeds
            .as_ref()
            .into_iter()
            .flat_map(|seeds| seeds.type17)
            .filter(|seed| !seed.skip())
        {
            let (die, channel) = match ctx.generation {
                Generation::Naples => (seed.channel() / 2, seed.channel() % 2),
                Generation::Rome | Generation::Milan => (0, seed.channel()),
            };
            let invalid = seed.socket() >= ctx.topology.sockets
                || seed.dimm() >= 2
                || !ctx
                    .topology
                    .nodes
                    .iter()
                    .any(|node| node.socket == seed.socket() && node.die == die)
                || dimms.iter().any(|dimm: &Dimm| {
                    dimm.socket == seed.socket()
                        && dimm.die == die
                        && dimm.device.channel == channel
                        && dimm.device.slot == seed.dimm()
                });
            if invalid {
                fstart_log::warn!(
                    "smbios: invalid/duplicate memory position; omitting memory inventory"
                );
                dimms.clear();
                break;
            }
            let device = if seed.present() {
                let table = mem::spd(&ctx.apob, seed.socket(), die).ok();
                let record = table.as_ref().and_then(|table| {
                    table
                        .dimms()
                        .find(|(c, d, _)| *c == channel && *d == seed.dimm())
                        .map(|(_, _, record)| record)
                });
                let configured = mem::mem_config(&ctx.apob, seed.socket(), die)
                    .ok()
                    .and_then(|config| config.memclk().ok())
                    .and_then(|mhz| mhz.checked_mul(2))
                    .unwrap_or(0);
                record.and_then(|record| fstart_driver_spd::ddr4::inventory(&record.spd, configured, channel, seed.dimm()).ok())
                    .unwrap_or_else(|| {
                        fstart_log::warn!("smbios: SPD unavailable/unsupported at socket {} die {} channel {} DIMM {}",
                            seed.socket(), die, channel, seed.dimm());
                        MemoryDevice::unknown(channel, seed.dimm())
                    })
            } else {
                MemoryDevice::empty(channel, seed.dimm())
            };
            dimms.push(Dimm {
                socket: seed.socket(),
                die,
                device,
            });
        }
        let map = sysmap::system_memory_map(&ctx.apob).or_halt("SMBIOS memory map");
        let regions = sysmap::memory_map(ctx.generation.into(), map.top, map.holes())
            .or_halt("SMBIOS DRAM ranges");
        let bytes = crate::flow::dma_alloc(smbios::BUFFER_SIZE, 16);
        let address = bytes.as_ptr() as u64;
        let used = build_into(
            bytes,
            address,
            &Inputs {
                identity: B::smbios_identity(),
                processors: &processors,
                dimms: &dimms,
                regions: &regions,
                // EPYC 7001: up to 128 GiB per DIMM; 7002/7003: up to
                // 256 GiB. Position count comes from the board's ABL seeds,
                // not eight/32 assumed slots or the installed DIMM sizes.
                max_dimm_mib: match ctx.generation {
                    Generation::Naples => 128 * 1024,
                    _ => 256 * 1024,
                },
                ecc: seeds.as_ref().is_some_and(|seeds| seeds.ecc()),
                rom_size,
                uefi: cfg!(fstart_payload = "crabefi"),
            },
        )
        .or_halt("SMBIOS tables");
        fstart_log::info!(
            "smbios: {} packages, {} positions, {} installed, {} bytes at {:#x}",
            processors.len() as u32,
            dimms.len() as u32,
            dimms.iter().filter(|d| d.device.is_populated()).count() as u32,
            used as u32,
            address
        );
        address
    }
}

#[cfg(test)]
mod tests;
