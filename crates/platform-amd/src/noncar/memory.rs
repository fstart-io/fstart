// SPDX-License-Identifier: MIT OR Apache-2.0
//! The memory map of a noncar SoC: the PSP's APOB and TOP_MEM/TOM2 replace
//! FSP HOBs.
use super::layout::Region;
use fstart_core::services::memory_detect::{
    E820Entry, E820Kind, E820State, build_pc_compatible_e820,
};
use fstart_driver_amd::apob::MemoryMap;

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

/// TSEG at the top of the DRAM below 4 GiB, aligned to its size.
pub fn tseg_base(apob: &MemoryMap, top_mem: u64, tseg_size: u64) -> u64 {
    low_top(apob, top_mem).saturating_sub(tseg_size) & !(tseg_size - 1)
}

/// The e820 map handed to the payload: APOB holes, TSEG and everything above
/// it, the `firmware` DRAM (the early region, the APOB) and the AP bootstrap
/// page reserved.
pub fn e820(
    apob: &MemoryMap,
    top_mem: u64,
    tseg_size: u64,
    firmware: &[Region],
) -> Result<E820State, Error> {
    let low = low_top(apob, top_mem);
    let tseg = tseg_base(apob, top_mem, tseg_size);
    if low > 1 << 32 || firmware.iter().any(|region| tseg < region.end()) {
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
    for region in firmware {
        reserve(&mut map, region.base, region.size)?;
    }
    reserve(&mut map, fstart_arch::x86::mp::AP_BOOTSTRAP_ADDRESS, 0x1000)?;
    reserve_iommu_range(&mut map, apob.top)?;
    let total = map
        .entries()
        .iter()
        .filter(|entry| entry.kind == E820Kind::Ram as u32)
        .map(|entry| entry.size)
        .sum();
    map.set_detected(map.count(), total)
        .map_err(|_| Error::Capacity)?;
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

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use fstart_driver_amd::apob::{self, Apob};
    use std::vec;

    fn apob_bytes(holes: &[(u64, u64, u32)]) -> std::vec::Vec<u8> {
        let mut bytes = vec![0; 16 + 48 + 16 + holes.len() * 24];
        let size = bytes.len() as u32;
        bytes[0..4].copy_from_slice(b"APOB");
        bytes[8..12].copy_from_slice(&size.to_le_bytes());
        bytes[12..16].copy_from_slice(&16u32.to_le_bytes());
        bytes[16..20].copy_from_slice(&9u32.to_le_bytes());
        bytes[20..24].copy_from_slice(&9u32.to_le_bytes());
        bytes[28..32].copy_from_slice(&(size - 16).to_le_bytes());
        bytes[64..72].copy_from_slice(&(16u64 << 30).to_le_bytes());
        bytes[72..76].copy_from_slice(&(holes.len() as u32).to_le_bytes());
        for (index, (base, size, kind)) in holes.iter().enumerate() {
            let offset = 80 + index * 24;
            bytes[offset..offset + 8].copy_from_slice(&base.to_le_bytes());
            bytes[offset + 8..offset + 16].copy_from_slice(&size.to_le_bytes());
            bytes[offset + 16..offset + 20].copy_from_slice(&kind.to_le_bytes());
        }
        bytes
    }

    const EARLY: Region = Region::new(0x7600_0000, 0x100_0000);
    const APOB: Region = Region::new(0x3000_0000, 0x2_0000);

    #[test]
    fn apob_holes_tseg_and_the_firmware_are_never_handed_to_the_os() {
        let holes = [
            (0xd000_0000u64, 0x3000_0000u64, 1u32),
            (0xc800_0000, 0x0800_0000, 0),
            (6 << 30, 0x0800_0000, 2),
            (0x2000_0000, 0x0100_0000, 99), // an unknown PSP reservation still isn't RAM
        ];
        let bytes = apob_bytes(&holes);
        let apob = Apob::new(&bytes, fstart_driver_amd::psp::Variant::Rv).unwrap();
        let apob = apob::sysmap::first_system_memory_map(&apob)
            .unwrap()
            .sorted()
            .unwrap();
        // The hole at 0xc8000000 ends above 1 GiB: TSEG goes below it.
        assert_eq!(low_top(&apob, 0xd000_0000), 0xc800_0000);
        let tseg = tseg_base(&apob, 0xd000_0000, 8 << 20);
        assert_eq!(tseg, 0xc780_0000);
        let map = e820(&apob, 0xd000_0000, 8 << 20, &[EARLY, APOB]).unwrap();
        let ram = |base: u64, size: u64| {
            map.entries().iter().any(|entry| {
                entry.kind == E820Kind::Ram as u32
                    && entry.addr < base + size
                    && entry.addr + entry.size > base
            })
        };
        for (base, size) in holes.iter().map(|&(base, size, _)| (base, size)).chain([
            (EARLY.base, EARLY.size),
            (APOB.base, APOB.size),
            (tseg, 8 << 20),
            (fstart_arch::x86::mp::AP_BOOTSTRAP_ADDRESS, 4096),
            IOMMU_RESERVED,
        ]) {
            assert!(!ram(base, size), "{base:#x}");
        }
        assert!(ram(0x1_0000_0000, 1) && ram(0x4000_0000, 0x1000));
        let total: u64 = map
            .entries()
            .iter()
            .filter(|entry| entry.kind == E820Kind::Ram as u32)
            .map(|entry| entry.size)
            .sum();
        assert_eq!(map.total_ram(), total);
        // TSEG can't go below the firmware.
        assert_eq!(
            e820(
                &apob,
                0xd000_0000,
                8 << 20,
                &[Region::new(0xc700_0000, 0x100_0000)]
            )
            .err(),
            Some(Error::Boundary)
        );
    }
}
