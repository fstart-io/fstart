// SPDX-License-Identifier: MIT OR Apache-2.0
//! CrabEFI as on the other boards: it enumerates the live roots' buses and
//! drives the controllers itself (xHCI, NVMe, AHCI), reading only the BARs
//! fstart assigned. fstart hands over powered, assigned, never-started
//! controllers, the final OS memory map, native ACPI and SMBIOS. No
//! persistent variables.
use alloc::vec::Vec;
use fstart_boot::crabefi::{self, MemoryRegion, MemoryType, PciEcamRegion, UefiLaunchConfig};
use fstart_driver_amd::{apob::sysmap::Region, df::v4::Root};
use fstart_driver_uart::ns16550::Ns16550;

pub fn launch<S: crate::Soc>(
    console: &Ns16550,
    regions: &[Region],
    roots: &[Root],
    rsdp: u64,
    tseg: Option<core::ops::Range<u64>>,
) -> ! {
    use fstart_core::services::memory_detect::E820Kind;
    let (name, layout) = (S::NAME, S::LAYOUT);
    let range = |r: crate::Region| r.base..r.base + r.size;
    // The reset image, its heap (ACPI, SMBIOS, MPIO/USB DMA) and stack stay
    // reserved, the APOB and TSEG.
    let reserved: Vec<_> = [range(layout.cache), range(layout.apob)]
        .into_iter()
        .chain(tseg)
        .collect();
    let e820 =
        fstart_platform_amd::rs_resources::os_memory_map(regions, &reserved).unwrap_or_else(|_| {
            fstart_log::error!("{}: CrabEFI memory map rejected", name);
            fstart_arch::halt()
        });
    // CrabEFI frees its transient buffers; fstart's bump heap would run out
    // reading an initramfs.
    let heap = range(layout.payload_heap);
    let mut map: Vec<MemoryRegion> = Vec::new();
    for e in e820.entries() {
        let (base, end) = (e.addr, e.addr + e.size);
        if e.kind != E820Kind::Ram as u32 {
            map.push(region(base, end, MemoryType::Reserved));
        } else if base <= heap.start && heap.end <= end {
            map.extend(
                [
                    (base, heap.start, MemoryType::Ram),
                    (heap.start, heap.end, MemoryType::BootServicesData),
                    (heap.end, end, MemoryType::Ram),
                ]
                .into_iter()
                .filter(|(b, e, _)| b < e)
                .map(|(b, e, kind)| region(b, e, kind)),
            );
        } else {
            map.push(region(base, end, MemoryType::Ram));
        }
    }
    if !map
        .iter()
        .any(|r| r.base == heap.start && r.region_type == MemoryType::BootServicesData)
    {
        fstart_log::error!(
            "{}: CrabEFI heap {:#x}..{:#x} is not RAM",
            name,
            heap.start,
            heap.end
        );
        fstart_arch::halt();
    }
    // SAFETY: proven RAM outside every firmware reservation and the bump
    // heap; nothing else owns it, and CrabEFI keeps it as boot-services data.
    unsafe { fstart_stage::adopt_freeing_heap(heap.start as usize..heap.end as usize) }
        .unwrap_or_else(|_| {
            fstart_log::error!("{}: CrabEFI heap adoption refused", name);
            fstart_arch::halt()
        });
    // Only the buses the DF routes: a fused IOS's range has no target.
    let ecam: Vec<_> = roots
        .iter()
        .map(|root| PciEcamRegion {
            base: fstart_pci::ecam::base() as u64 + (u64::from(root.route.base) << 20),
            segment: root.route.segment.into(),
            bus_start: root.route.base,
            bus_end: root.route.limit,
        })
        .collect();
    fstart_log::info!(
        "{}: launching CrabEFI over {} PCI root(s), RAM={:#x}; volatile variables",
        name,
        ecam.len(),
        e820.total_ram()
    );
    fstart_log::flush();
    let acpi_s5 = [fstart_core::description::AcpiSleepControl::new(
        S::PMBASE + 4,
        fstart_platform_amd::rs_acpi::S5_SLEEP_TYPE,
    )];
    crabefi::launch_x86_memory_map(
        UefiLaunchConfig {
            acpi_devices: &[],
            acpi_s5: &acpi_s5,
            console: Some(console),
            peripherals: Vec::new(),
            framebuffer: None,
            acpi_rsdp: Some(rsdp),
            smbios: Some(fstart_acpi::smbios::LEGACY_ENTRY_POINT),
            fdt: None,
            ecam_regions: &ecam,
        },
        &map,
    )
}

fn region(base: u64, end: u64, region_type: MemoryType) -> MemoryRegion {
    MemoryRegion {
        base,
        size: end - base,
        region_type,
    }
}
