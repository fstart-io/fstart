// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native memory/resource contract. APOB and AMD TOP_MEM replace FSP HOBs.
use crate::layout;
use fstart_core::services::{
    ServiceError,
    memory_detect::{E820Entry, E820Kind, E820State, MemoryDetector, build_pc_compatible_e820},
};
use fstart_driver_amd::apob::MemoryMap;

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
        if tseg_base <= layout::EARLY_END {
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
        // The whole early firmware region (image, data, stacks, microcode and
        // PSP mailbox pages) and the AP bootstrap page must be APOB/TOM RAM.
        let trampoline = fstart_arch::x86::mp::AP_BOOTSTRAP_ADDRESS;
        let usable = |base, end| {
            map.entries().iter().any(|entry| {
                entry.kind == E820Kind::Ram as u32
                    && entry.addr <= base
                    && entry
                        .addr
                        .checked_add(entry.size)
                        .is_some_and(|top| top >= end)
            })
        };
        if !usable(layout::EARLY_START, layout::EARLY_END) || !usable(trampoline, trampoline + 4096)
        {
            return Err(Error::Boundary);
        }
        // Firmware keeps all of it (parked APs, PSP mailbox, microcode) to reset.
        map.reserve_range(layout::EARLY_START, layout::EARLY_END - layout::EARLY_START)
            .map_err(|_| Error::Capacity)?;
        // AP reset dispatch and exception handlers also retain the complete low
        // bootstrap page through reset; leaving it as RAM permits payload reuse.
        map.reserve_range(trampoline, 4096)
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
    /// Numerical witness only; no physical ownership/authentication implication.
    pub(crate) fn microcode_reserved(&self) -> bool {
        self.reserved(layout::MICROCODE_START, layout::MICROCODE_END)
    }
    /// Numerical whole-page witness, not physical decoding/cache/DMA isolation.
    pub(crate) fn psp_reserved(&self) -> bool {
        self.reserved(layout::PSP_START, layout::PSP_END)
    }
    fn reserved(&self, base: u64, end: u64) -> bool {
        self.map.entries().iter().any(|entry| {
            entry.kind == E820Kind::Reserved as u32
                && entry.addr <= base
                && entry
                    .addr
                    .checked_add(entry.size)
                    .is_some_and(|top| top >= end)
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
