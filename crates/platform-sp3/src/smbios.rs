//! SP3 inventory: the shared AMD SMBIOS builder
//! (`fstart_driver_amd::apob::smbios`) filled from the SP3 APOB and CPUs.

extern crate alloc;

#[cfg(feature = "bundle-reset")]
use fstart_acpi::smbios;
#[cfg(feature = "bundle-reset")]
use fstart_arch::x86::boot::cpuid::ProcessorSnapshot;
pub use fstart_driver_amd::apob::smbios::{Dimm, DramRange, Inputs, Processor, build_into};

#[cfg(feature = "bundle-reset")]
pub(crate) mod runtime {
    use super::*;
    use crate::flow::{Ctx, OrHalt};
    use alloc::vec::Vec;
    use fstart_driver_amd::{
        apob::{
            mem,
            smbios::{dimms as smbios_dimms, dram_ranges},
            sysmap,
        },
        sp3::{ccx::topology::ApicLayout, soc::Generation},
    };

    /// Capture package-local CPUID before APs are parked for the payload and
    /// publish immutable tables in the existing reserved firmware heap.
    /// `rom_size` is the image extent recorded by the flash anchor, not a
    /// possibly partial EFS mapping or a physical flash-geometry measurement.
    pub(crate) fn prepare(
        ctx: &Ctx,
        mp: &fstart_arch::x86::mp::MpHandle,
        rom_size: u32,
        identity: fstart_acpi::smbios::SmbiosIdentity<'_>,
    ) -> u64 {
        let smt = crate::mp::this_thread(ctx.generation).smt();
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
        let dimms = smbios_dimms(
            &ctx.apob,
            match ctx.generation {
                Generation::Naples => 2,
                Generation::Rome | Generation::Milan => 8,
            },
            |socket, die| {
                socket < ctx.topology.sockets
                    && ctx
                        .topology
                        .nodes
                        .iter()
                        .any(|node| node.socket == socket && node.die == die)
            },
        );
        let map = sysmap::system_memory_map(&ctx.apob).or_halt("SMBIOS memory map");
        let regions: Vec<DramRange> = dram_ranges(
            &sysmap::memory_map(ctx.generation.into(), map.top, map.holes())
                .or_halt("SMBIOS DRAM ranges"),
        );
        let bytes = crate::flow::dma_alloc(smbios::BUFFER_SIZE, 16);
        let address = bytes.as_ptr() as u64;
        let used = build_into(
            bytes,
            address,
            &Inputs {
                identity,
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
        // For Linux without UEFI.
        // SAFETY: `cache_ram` routed C0000-FFFFF to DRAM and the e820
        // reserves it (`memory::LEGACY_HOLE`); nothing else lives there.
        unsafe { smbios::publish_legacy_entry_point(bytes) };
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
