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

use fstart_core::mmio::{MmioReadOnly, MmioReadWrite};
use tock_registers::interfaces::{Readable, Writeable};
use tock_registers::{register_bitfields, register_structs};

/// The FU740's cache controller.
const FU740_CCACHE_BASE: usize = 0x0201_0000;

register_bitfields! [u32,
    /// Config, 0x000, 32 bits: the cache geometry.
    Config [
        BANKS OFFSET(0) NUMBITS(8) [],
        WAYS OFFSET(8) NUMBITS(8) [],
        LG_SETS OFFSET(16) NUMBITS(8) [],
        LG_BLOCK_BYTES OFFSET(24) NUMBITS(8) [],
    ],
];

register_structs! {
    CcacheRegs {
        /// Config, 0x000, 32 bits.
        (0x000 => config: MmioReadOnly<u32, Config::Register>),
        (0x004 => _reserved0),
        /// WayEnable, 0x008, 32 bits: index of the largest way enabled
        /// for caching.
        (0x008 => wayenable: MmioReadWrite<u32>),
        (0x00c => @END),
    }
}

fn regs() -> &'static CcacheRegs {
    // SAFETY: the FU740 cache controller's fixed register block.
    unsafe { &*(FU740_CCACHE_BASE as *const CcacheRegs) }
}

/// Enable every L2 way for caching, retiring the LIM aperture.
///
/// Call only once no code or live data remains in LIM (the DRAM stage,
/// before payload loads). Earlier loads must be globally visible first:
/// the aperture's contents stop being addressable as ways come up.
pub fn enable_all_ways() {
    let regs = regs();
    let ways = regs.config.read(Config::WAYS);
    if ways == 0 {
        return;
    }
    compiler_fence(Ordering::SeqCst);
    regs.wayenable.set(ways - 1);
    compiler_fence(Ordering::SeqCst);
}
