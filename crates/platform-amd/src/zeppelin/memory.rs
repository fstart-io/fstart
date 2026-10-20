// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Zeppelin memory map: the PSP's APOB and TOP_MEM/TOM2 replace FSP HOBs.
use super::layout;
use fstart_core::services::memory_detect::{
    E820Entry, E820Kind, E820State, build_pc_compatible_e820,
};
use fstart_driver_amd::{apob::MemoryMap, zeppelin as soc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Boundary,
    Capacity,
}

/// The lowest hole below 4 GiB ending above 1 GiB starts the MMIO/reserved
/// region.
pub fn low_top(apob: &MemoryMap, top_mem: u64) -> u64 {
    apob.low_top_above(1 << 30).min(top_mem)
}

/// 8 MiB of TSEG.
pub const TSEG_SIZE: u32 = 8 << 20;

/// TSEG at the top of the DRAM below 4 GiB, aligned to its size.
pub fn tseg_base(apob: &MemoryMap, top_mem: u64) -> u64 {
    let size = u64::from(TSEG_SIZE);
    low_top(apob, top_mem).saturating_sub(size) & !(size - 1)
}

/// The e820 map handed to the payload: APOB holes, TSEG and everything above
/// it, and the firmware's own DRAM (early region, APOB, AP bootstrap page)
/// reserved.
pub fn e820(apob: &MemoryMap, top_mem: u64) -> Result<E820State, Error> {
    let low = low_top(apob, top_mem);
    let tseg = tseg_base(apob, top_mem);
    if low > 1 << 32 || tseg < layout::EARLY_END {
        return Err(Error::Boundary);
    }
    let mut map = E820State::new();
    let count = build_pc_compatible_e820(map.entries_mut(), low as u32, apob.top, low as u32)
        .map_err(|_| Error::Capacity)?;
    map.set_detected(count, 0).map_err(|_| Error::Capacity)?;
    let reserve = |map: &mut E820State, base: u64, size: u64| {
        map.reserve_range(base, size).map_err(|_| Error::Capacity)
    };
    for hole in apob.holes() {
        // Unknown APOB hole types are still not RAM.
        reserve(&mut map, hole.base, hole.size)?;
    }
    reserve(&mut map, tseg, low - tseg)?;
    // Parked APs, firmware tables and DMA buffers live here until reset.
    reserve(&mut map, layout::EARLY_START, layout::EARLY_END - layout::EARLY_START)?;
    reserve(&mut map, soc::APOB_BASE, soc::APOB_SIZE as u64)?;
    reserve(&mut map, fstart_arch::x86::mp::AP_BOOTSTRAP_ADDRESS, 0x1000)?;
    reserve_iommu_range(&mut map, apob.top)?;
    let total = map
        .entries()
        .iter()
        .filter(|entry| entry.kind == E820Kind::Ram as u32)
        .map(|entry| entry.size)
        .sum();
    map.set_detected(map.count(), total).map_err(|_| Error::Capacity)?;
    Ok(map)
}

/// The fixed IOMMU range stays reserved: the old
/// HyperTransport window below 1 TiB, where the IOMMU and the fabric decode
/// interrupts and system management instead of DRAM or PCI MMIO.
const IOMMU_RESERVED: (u64, u64) = (0xfd_0000_0000, 4 << 30);

fn reserve_iommu_range(map: &mut E820State, dram_top: u64) -> Result<(), Error> {
    let (base, size) = IOMMU_RESERVED;
    if dram_top > base {
        return map.reserve_range(base, size).map_err(|_| Error::Capacity);
    }
    let count = map.count();
    *map.entries_mut().get_mut(count).ok_or(Error::Capacity)? =
        E820Entry::new(base, size, E820Kind::Reserved);
    map.set_detected(count + 1, 0).map_err(|_| Error::Capacity)
}
