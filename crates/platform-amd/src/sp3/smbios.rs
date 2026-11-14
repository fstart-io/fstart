//! The SMBIOS tables of SP3 ([`crate::smbios`]): a package per socket, the
//! dies the fabric found, the DRAM ranges of the APOB's memory map, built
//! in the reserved firmware heap.

use crate::smbios::SocSmbios;
use crate::sp3::silicon::Generation;

/// What the tables of `generation` differ in: socket SP3, 2 channels per
/// Naples die and 8 per Rome/Milan socket, and DIMMs up to 128 GiB on EPYC
/// 7001 and 256 GiB on 7002/7003.
pub const fn of(generation: Generation) -> SocSmbios {
    let naples = matches!(generation, Generation::Naples);
    SocSmbios {
        socket_upgrade: 0x37,
        max_dimm_mib: if naples { 128 * 1024 } else { 256 * 1024 },
        channels_per_die: if naples { 2 } else { 8 },
    }
}

/// Build the tables before the APs are parked for the payload; returns
/// their entry point's address. `rom_size` is the image extent recorded by
/// the flash anchor, not a possibly partial EFS mapping or a physical
/// flash-geometry measurement.
#[cfg(all(feature = "bundle", target_os = "none"))]
pub(crate) fn install(
    ctx: &crate::sp3::flow::Ctx<'_>,
    cpus: &crate::mp::Cpus,
    rom_size: u32,
    identity: &fstart_acpi::smbios::SmbiosIdentity<'_>,
    pci: &fstart_pci::PciEcam,
) -> u64 {
    use crate::sp3::flow::OrHalt;
    use fstart_driver_amd::apob::{smbios::dram_ranges, sysmap};
    use fstart_driver_amd::ccx::topology::ApicLayout;

    let soc = ctx.soc;
    let smt = crate::sp3::silicon::cpu::this_thread(soc.generation).smt();
    let layout = ApicLayout::new(soc.generation, smt);
    let map = sysmap::system_memory_map(&soc.apob).or_halt("SMBIOS memory map");
    let regions = dram_ranges(
        &sysmap::memory_map(soc.generation.into(), map.top, map.holes())
            .or_halt("SMBIOS DRAM ranges"),
    );
    let bytes = crate::sp3::silicon::steps::dma_alloc(fstart_acpi::smbios::BUFFER_SIZE, 16)
        .or_halt("DMA allocation");
    let address = bytes.as_ptr() as u64;
    crate::smbios::build_into(
        bytes,
        &crate::smbios::System {
            soc: &of(Generation::of(soc.generation)),
            identity,
            rom_size,
            apob: Some(&soc.apob),
            regions: &regions,
            cpus,
            pci,
            bridges: crate::sp3::acpi::of(Generation::of(soc.generation)).bridges,
            ports: &soc.ports,
        },
        |apic_id| (layout.decode(apic_id).socket, apic_id >> u32::from(smt)),
        |socket, die| {
            let nodes = &soc.topology.nodes;
            socket < soc.topology.sockets
                && nodes
                    .iter()
                    .any(|node| node.socket == socket && node.die == die)
        },
    )
    .or_halt("SMBIOS tables");
    address
}
