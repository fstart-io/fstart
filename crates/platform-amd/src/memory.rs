// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native memory/resource contract. APOB and AMD TOP_MEM replace FSP HOBs.
use fstart_core::services::{
    ServiceError,
    memory_detect::{E820Entry, E820Kind, E820State, MemoryDetector, build_pc_compatible_e820},
};
use fstart_driver_amd::picasso::apob::MemoryMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Boundary,
    Capacity,
}

pub struct Memory {
    map: E820State,
    /// End of allocatable low RAM; TSEG/UMA and alignment slack stay reserved.
    pub allocation_top: u64,
    /// Native SMM reservation, which the later CPU/SMM flow must program.
    pub tseg_base: u64,
    pub tseg_size: u64,
}
impl Memory {
    pub fn from_apob(
        apob: &MemoryMap,
        top_mem: u64,
        top_mem2: Option<u64>,
        tseg_size: u64,
    ) -> Result<Self, Error> {
        let ceiling = top_mem2.filter(|&top| top != 0).unwrap_or(1 << 32);
        if ceiling < 1 << 32
            || apob.top > ceiling
            || top_mem == 0
            || top_mem >= 1 << 32
            || apob.top < top_mem
            || tseg_size < 0x100000
            || !tseg_size.is_power_of_two()
        {
            return Err(Error::Boundary);
        }
        let top = apob.low_top().min(top_mem);
        let tseg_base =
            top.checked_sub(tseg_size).ok_or(Error::Boundary)? & !(tseg_size.max(0x800000) - 1);
        if tseg_base <= 0x02050000 {
            return Err(Error::Boundary);
        }
        let mut map = E820State::new();
        let count = build_pc_compatible_e820(
            map.entries_mut(),
            tseg_base as u32,
            apob.top,
            top_mem as u32,
        )
        .map_err(|_| Error::Capacity)?;
        map.set_detected(count, 0).map_err(|_| Error::Capacity)?;
        for hole in apob.holes() {
            // Unknown APOB types are still non-RAM. Do not drop privileged,
            // UMA or coprocessor reservations just because x86 doesn't name them.
            map.reserve_range(hole.base, hole.size)
                .map_err(|_| Error::Capacity)?;
        }
        // Exclude the entire PSP/early-firmware window, not just executable bytes.
        map.reserve_range(0x02000000, 0x50000)
            .map_err(|_| Error::Capacity)?;
        let total = map
            .entries()
            .iter()
            .filter(|entry| entry.kind == E820Kind::Ram as u32)
            .map(|entry| entry.size)
            .sum();
        map.set_detected(map.count(), total)
            .map_err(|_| Error::Capacity)?;
        Ok(Self {
            map,
            allocation_top: tseg_base,
            tseg_base,
            tseg_size,
        })
    }
    pub fn state(&self) -> &E820State {
        &self.map
    }
    pub fn into_state(self) -> E820State {
        self.map
    }
}
impl MemoryDetector for Memory {
    fn detect_memory(&self, entries: &mut [E820Entry]) -> Result<usize, ServiceError> {
        let count = self.map.count();
        entries
            .get_mut(..count)
            .ok_or(ServiceError::InvalidParam)?
            .copy_from_slice(self.map.entries());
        Ok(count)
    }
    fn total_ram_bytes(&self) -> Result<u64, ServiceError> {
        Ok(self.map.total_ram())
    }
}
