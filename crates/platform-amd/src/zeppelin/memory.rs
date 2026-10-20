// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Zeppelin memory map: the PSP's APOB and TOP_MEM/TOM2 replace FSP HOBs.
use super::layout;
use fstart_core::services::memory_detect::{E820Kind, E820State, build_pc_compatible_e820};
use fstart_driver_amd::{fam17h::apob::MemoryMap, zeppelin as soc};

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

/// The e820 map handed to the payload: APOB holes and the firmware's own
/// DRAM (early region, APOB, AP bootstrap page) reserved.
pub fn e820(apob: &MemoryMap, top_mem: u64) -> Result<E820State, Error> {
    let low = low_top(apob, top_mem);
    if low <= layout::EARLY_END || low > 1 << 32 {
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
    // Parked APs, firmware tables and DMA buffers live here until reset.
    reserve(&mut map, layout::EARLY_START, layout::EARLY_END - layout::EARLY_START)?;
    reserve(&mut map, soc::APOB_BASE, soc::APOB_SIZE as u64)?;
    reserve(&mut map, fstart_arch::x86::mp::AP_BOOTSTRAP_ADDRESS, 0x1000)?;
    let total = map
        .entries()
        .iter()
        .filter(|entry| entry.kind == E820Kind::Ram as u32)
        .map(|entry| entry.size)
        .sum();
    map.set_detected(map.count(), total).map_err(|_| Error::Capacity)?;
    Ok(map)
}
