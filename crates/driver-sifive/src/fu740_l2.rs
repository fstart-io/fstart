//! FU740 composable cache (CCache) controller: just enough to hand the
//! whole L2 back to caching once firmware no longer runs from LIM.
//!
//! The 2 MiB L2 SRAM is split by way between cache and the loosely
//! integrated memory (LIM) window at 0x0800_0000: ways above
//! `wayenable` back the LIM aperture. Reset enables way 0 only, which
//! is what makes most of the aperture usable SRAM for the FSBL the
//! mask ROM loads there. Nothing may still execute from or keep state
//! in LIM once more ways are enabled.

use core::sync::atomic::{Ordering, compiler_fence};

const FU740_CCACHE_BASE: u64 = 0x0201_0000;

const CONFIG: usize = 0x000;
const WAYENABLE: usize = 0x008;

/// `config` bits [15:8]: number of ways.
const CONFIG_WAYS_SHIFT: u32 = 8;
const CONFIG_WAYS_MASK: u32 = 0xff << CONFIG_WAYS_SHIFT;

fn read(offset: usize) -> u32 {
    // SAFETY: the FU740 cache controller registers have a fixed, aligned
    // MMIO address.
    unsafe { fstart_core::mmio::read32((FU740_CCACHE_BASE as usize + offset) as *const u32) }
}

fn write(offset: usize, value: u32) {
    // SAFETY: same fixed MMIO window as `read`.
    unsafe { fstart_core::mmio::write32((FU740_CCACHE_BASE as usize + offset) as *mut u32, value) }
}

/// Enable every L2 way for caching, retiring the LIM aperture.
///
/// Call only once no code or live data remains in LIM (the DRAM stage,
/// before payload loads). Earlier loads must be globally visible first:
/// the aperture's contents stop being addressable as ways come up.
pub fn enable_all_ways() {
    let ways = (read(CONFIG) & CONFIG_WAYS_MASK) >> CONFIG_WAYS_SHIFT;
    if ways == 0 {
        return;
    }
    compiler_fence(Ordering::SeqCst);
    // wayenable holds the index of the largest enabled way.
    write(WAYENABLE, ways - 1);
    compiler_fence(Ordering::SeqCst);
}
