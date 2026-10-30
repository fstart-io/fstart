//! SP3 inventory: the shared AMD SMBIOS builder
//! (`fstart_platform_amd::smbios`) filled from the SP3 APOB and CPUs.

extern crate alloc;

#[cfg(feature = "bundle-reset")]
use fstart_acpi::smbios;
#[cfg(feature = "bundle-reset")]
use fstart_arch::x86::boot::cpuid::ProcessorSnapshot;
#[cfg(feature = "bundle-reset")]
use fstart_core::memory_info::MemoryDevice;
pub use fstart_platform_amd::smbios::{Dimm, DramRange, Inputs, Processor, build_into};

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
        for seed in seeds.iter().flat_map(|seeds| seeds.positions()) {
            let (die, channel) = match ctx.generation {
                Generation::Naples => (seed.channel / 2, seed.channel % 2),
                Generation::Rome | Generation::Milan => (0, seed.channel),
            };
            let invalid = seed.socket >= ctx.topology.sockets
                || seed.dimm >= 2
                || !ctx
                    .topology
                    .nodes
                    .iter()
                    .any(|node| node.socket == seed.socket && node.die == die)
                || dimms.iter().any(|dimm: &Dimm| {
                    dimm.socket == seed.socket
                        && dimm.die == die
                        && dimm.device.channel == channel
                        && dimm.device.slot == seed.dimm
                });
            if invalid {
                fstart_log::warn!(
                    "smbios: invalid/duplicate memory position; omitting memory inventory"
                );
                dimms.clear();
                break;
            }
            let device = if seed.present {
                let table = mem::spd(&ctx.apob, seed.socket, die).ok();
                let record = table.as_ref().and_then(|table| {
                    table
                        .dimms()
                        .find(|(c, d, _)| *c == channel && *d == seed.dimm)
                        .map(|(_, _, record)| record)
                });
                let configured = mem::mem_config(&ctx.apob, seed.socket, die)
                    .ok()
                    .and_then(|config| config.memclk().ok())
                    .and_then(|mhz| mhz.checked_mul(2))
                    .unwrap_or(0);
                record.and_then(|record| fstart_driver_spd::ddr4::inventory(&record.spd, configured, channel, seed.dimm).ok())
                    .unwrap_or_else(|| {
                        fstart_log::warn!("smbios: SPD unavailable/unsupported at socket {} die {} channel {} DIMM {}",
                            seed.socket, die, channel, seed.dimm);
                        MemoryDevice::unknown(channel, seed.dimm)
                    })
            } else {
                MemoryDevice::empty(channel, seed.dimm)
            };
            dimms.push(Dimm {
                socket: seed.socket,
                die,
                device,
            });
        }
        let map = sysmap::system_memory_map(&ctx.apob).or_halt("SMBIOS memory map");
        let regions: Vec<DramRange> =
            sysmap::memory_map(ctx.generation.into(), map.top, map.holes())
                .or_halt("SMBIOS DRAM ranges")
                .iter()
                .filter(|region| region.kind.is_ram())
                .map(|region| DramRange {
                    base: region.base,
                    end: region.end(),
                })
                .collect();
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
                socket_upgrade: 0x37, // Socket SP3
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
