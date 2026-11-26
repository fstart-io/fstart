// SPDX-License-Identifier: MIT OR Apache-2.0
//! A CXL endpoint's decoders from what the ABL mapped (CxlV2Manager
//! `CxlMgrCompareApobToApcb` `0x1DD0`, `CxlMgrV2EnableMemoryPool`
//! `0x2C90`, `CxlMgrEndpointDecoderProgramming` `0xC5B0`;
//! reports/research/CXL_COMPLETE.md §2.2.1, §2.2.2).
//!
//! Pool `n` of an endpoint is the `n`-th APCB region of its address (or its
//! top switch's); its window is the APOB window that interleaves that
//! region, on the same socket, of the same memory. With system-address
//! decoding (`PcdAmdCxlSpaEnable`, both donors) the decoder takes the
//! whole window and its interleave; else AMD's normalized addressing: base
//! 0, the region's size, one way (the OS then needs AMD's address
//! translation).

use fstart_amdfw_format::apcb::CxlRegion;
use heapless::Vec;

use crate::apob::cxl::{Memory, Window};

/// Most decoders fstart commits on one endpoint.
pub const MAX_DECODERS: usize = 4;

/// The decoder of one pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    /// The pool (and the endpoint decoder) index.
    pub pool: usize,
    /// The APCB region and the APOB window it is in.
    pub region: usize,
    pub window: usize,
    pub base: u64,
    pub size: u64,
    pub ways: u8,
    /// Interleave granularity in bytes.
    pub granularity: u32,
    pub memory: Memory,
}

impl Plan {
    /// A persistent pool, for which the SMU is told first
    /// ([`super::enable_scm`]: pool types 1 and 3).
    pub fn persistent(&self) -> bool {
        matches!(self.memory, Memory::Persistent | Memory::OtherPersistent)
    }
}

/// The APOB window type of an APCB region's memory.
fn same_memory(region: &CxlRegion, window: &Window) -> bool {
    let raw = match window.memory {
        Memory::Volatile => 0,
        Memory::Persistent => 1,
        Memory::DeviceCoherent => 2,
        Memory::OtherPersistent => 3,
        Memory::Unknown(other) => other,
    };
    raw == region.memory as u8
}

/// The decoders of the endpoint at `address` `(segment, bus, device,
/// function)` with `pools` pools, behind the top switch at `top` if any,
/// on `socket`: one per pool the ABL mapped, in pool order.
pub fn plan(
    regions: &[CxlRegion],
    windows: &[Window],
    address: (u8, u8, u8, u8),
    top: Option<(u8, u8, u8, u8)>,
    pools: usize,
    socket: u8,
    spa: bool,
) -> Vec<Plan, MAX_DECODERS> {
    let key = |r: &CxlRegion| (r.segment, r.bus, r.device, r.function);
    let mine: Vec<usize, { fstart_amdfw_format::apcb::MAX_CXL_REGIONS }> = regions
        .iter()
        .enumerate()
        .filter(|(_, r)| r.size != 0 && (key(r) == address || Some(key(r)) == top))
        .map(|(i, _)| i)
        .collect();
    let mut out = Vec::new();
    for (pool, &index) in mine.iter().enumerate().take(pools.min(MAX_DECODERS)) {
        let region = &regions[index];
        let Some((w, window)) = windows.iter().enumerate().find(|(_, w)| {
            w.status == 0
                && w.base != u64::MAX
                && w.size != 0
                && w.socket == socket
                && same_memory(region, w)
                && w.targets.contains(&(index as u8))
        }) else {
            continue;
        };
        let (base, size, ways) = if spa {
            (window.base, window.size, window.targets.len().max(1) as u8)
        } else {
            (0, region.size, 1)
        };
        let _ = out.push(Plan {
            pool,
            region: index,
            window: w,
            base,
            size,
            ways,
            granularity: super::host::granularity(window.interleave_size),
            memory: window.memory,
        });
    }
    out
}

/// Whether `regions` can be the array the ABL mapped `windows` from this
/// boot: every window [`plan`] would take (status 0, a base, a size)
/// interleaves distinct regions that exist, have a size, sit on the
/// window's socket, hold its memory, and together fit in it. An array
/// from another boot (a slot the PSP did not boot, a write that failed
/// halfway) would match pools to the wrong windows; the caller then
/// enables none. fstart's check: the vendor trusts the APCB it booted.
pub fn consistent(regions: &[CxlRegion], windows: &[Window]) -> bool {
    windows
        .iter()
        .filter(|w| w.status == 0 && w.base != u64::MAX && w.size != 0)
        .all(|w| {
            let mut total: u64 = 0;
            w.targets.iter().enumerate().all(|(i, &t)| {
                let Some(r) = regions.get(usize::from(t)) else {
                    return false;
                };
                total = total.saturating_add(r.size);
                !w.targets[..i].contains(&t)
                    && r.size != 0
                    && r.socket == w.socket
                    && same_memory(r, w)
            }) && !w.targets.is_empty()
                && total <= w.size
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_amdfw_format::apcb::CxlMemory;

    const G: u64 = 1 << 30;

    fn region(bus: u8, size: u64) -> CxlRegion {
        CxlRegion {
            size,
            alignment: 0x20,
            socket: 0,
            die: 0,
            nbio_map: 2,
            interleave_size: 0xff,
            sub_interleave_map: [1; 4],
            sub_interleave_size: 0xff,
            memory: CxlMemory::Volatile,
            device_type: 3,
            link_speed: 5,
            flit: 0,
            segment: 0,
            bus,
            device: 0,
            function: 0,
            status: 0,
        }
    }

    fn window(base: u64, size: u64, targets: &[u8]) -> Window {
        Window {
            base,
            size,
            status: 0,
            socket: 0,
            nbio_map: 2,
            alignment: 32,
            interleave_size: 0,
            sub_interleave_map: [0; 4],
            sub_interleave_size: 0xff,
            memory: Memory::Volatile,
            targets: Vec::from_slice(targets).unwrap(),
        }
    }

    #[test]
    fn pools_take_their_windows() {
        // Two expanders interleaved two ways in one window; a third alone.
        let regions = [
            region(0x41, 16 * G),
            region(0x42, 16 * G),
            region(0x43, 8 * G),
        ];
        let windows = [
            window(0x400 * G, 32 * G, &[0, 1]),
            window(0x500 * G, 8 * G, &[2]),
        ];
        let p = plan(&regions, &windows, (0, 0x42, 0, 0), None, 1, 0, true);
        assert_eq!(
            p[..],
            [Plan {
                pool: 0,
                region: 1,
                window: 0,
                base: 0x400 * G,
                size: 32 * G,
                ways: 2,
                granularity: 256,
                memory: Memory::Volatile
            }]
        );
        // Normalized: base 0, the region's size, one way.
        let n = plan(&regions, &windows, (0, 0x43, 0, 0), None, 1, 0, false);
        assert_eq!((n[0].base, n[0].size, n[0].ways), (0, 8 * G, 1));
        // Another socket's window, or none: no decoder.
        assert!(plan(&regions, &windows, (0, 0x43, 0, 0), None, 1, 1, true).is_empty());
        assert!(plan(&regions, &windows, (0, 0x44, 0, 0), None, 1, 0, true).is_empty());
        // Behind a switch: the top switch's region.
        let e = plan(
            &regions,
            &windows,
            (0, 0x60, 0, 0),
            Some((0, 0x43, 0, 0)),
            1,
            0,
            true,
        );
        assert_eq!(e[0].window, 1);
        assert!(!e[0].persistent());
    }

    #[test]
    fn only_the_booted_array_matches_the_windows() {
        let regions = [
            region(0x41, 16 * G),
            region(0x42, 16 * G),
            region(0x43, 8 * G),
        ];
        let windows = [
            window(0x400 * G, 32 * G, &[0, 1]),
            window(0x500 * G, 8 * G, &[2]),
        ];
        assert!(consistent(&regions, &windows));
        // No windows: nothing to match wrongly.
        assert!(consistent(&[], &[]));
        // Another boot's array: a region gone, grown, or of another kind.
        assert!(!consistent(&regions[..2], &windows));
        let mut grown = regions;
        grown[2].size = 16 * G;
        assert!(!consistent(&grown, &windows));
        let mut excluded = regions;
        excluded[1].size = 0;
        assert!(!consistent(&excluded, &windows));
        let mut pmem = regions;
        pmem[2].memory = CxlMemory::Persistent;
        assert!(!consistent(&pmem, &windows));
        let mut elsewhere = regions;
        elsewhere[0].socket = 1;
        assert!(!consistent(&elsewhere, &windows));
        // A window the ABL could not map is not matched at all.
        let mut unmapped = windows.clone();
        unmapped[1].status = crate::apob::cxl::STATUS_OUT_OF_MEMORY_MAP;
        assert!(consistent(&regions[..2], &unmapped));
        // A target named twice.
        let twice = [window(0x400 * G, 32 * G, &[0, 0])];
        assert!(!consistent(&regions, &twice));
    }
}
