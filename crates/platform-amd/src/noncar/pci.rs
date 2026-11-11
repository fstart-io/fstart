// SPDX-License-Identifier: MIT OR Apache-2.0
//! The PCI root complex of a die as a PCI root bridge: the buses and windows
//! the DF routes to it, and what of them the firmware keeps. The allocator
//! and the ACPI host bridge of the die both take them from here.
use super::layout::Region;
use fstart_driver_amd::df::map::RootComplex;
use fstart_pci::{PciRoot, PciRootInfo, PciWindow};

/// Legacy LPC/FCH I/O: forwarded like the rest, not for PCI devices.
const LEGACY_IO: (u64, u64) = (0, 0x1000);
/// The IOAPIC, HPET and FCH fixed MMIO and everything above.
const FIXED_MMIO: u64 = 0xfec0_0000;
/// The top 12 GiB of the 48-bit address space are the fabric's own and
/// cannot hold MMIO (coreboot `DF_RESERVED_TOP_12GB_MMIO_SIZE`).
const FABRIC_RESERVED: u64 = (1 << 48) - (12 << 30);

/// A die's root complex as a PCI root bridge.
pub type Root = PciRoot;

/// The root bridge of the root complex `routes`, with its buses in the ECAM
/// `ecam`.
///
/// Its windows are what the DF routes to the die: its I/O, its MMIO below
/// the fixed MMIO without the ECAM window (the bridge's own, not a window),
/// and its MMIO above 4 GiB below the fabric's reserved top. The firmware
/// keeps the legacy I/O and `non_pci`, the pool of the NBIO's own MMIO;
/// the allocator has the rest and fills the low windows first.
///
/// `None` if the buses are outside the ECAM or the DF routes more windows
/// than a root bridge holds.
pub fn root(routes: &RootComplex, ecam: Region, non_pci: Region) -> Option<Root> {
    let last_bus = u8::try_from((ecam.size >> 20).checked_sub(1)?).ok()?;
    let buses = *routes.buses.start()..=(*routes.buses.end()).min(last_bus);
    if buses.is_empty() {
        return None;
    }
    let span = |&(base, size): &(u64, u64)| (base, base + size);
    let io = || {
        let routed = routes.io.iter().map(span);
        routed.map(|(lo, hi)| (lo, hi.min(0x1_0000)))
    };
    // The windows below 4 GiB first: the allocator fills them in this order.
    let below = |&(_, hi): &(u64, u64)| hi <= 1 << 32;
    let mmio = || {
        let routed = || routes.mmio.iter().map(span);
        let low = routed()
            .filter(below)
            .flat_map(|(lo, hi)| cut((lo, hi.min(FIXED_MMIO)), (ecam.base, ecam.end())));
        let high = routed()
            .filter(move |route| !below(route))
            .map(|(lo, hi)| (lo, hi.min(FABRIC_RESERVED)));
        low.chain(high)
    };
    // The part of a window the firmware keeps.
    let kept = |(keep_lo, keep_hi): (u64, u64)| {
        move |(lo, hi): (u64, u64)| (lo.max(keep_lo), hi.min(keep_hi))
    };
    let window = |make: fn(u64, u64) -> PciWindow| {
        move |(lo, hi): (u64, u64)| make(lo, hi.saturating_sub(lo))
    };
    PciRoot::new(
        PciRootInfo {
            segment: 0,
            ecam_base: ecam.base,
            bus_start: *buses.start(),
            bus_end: *buses.end(),
        },
        io().map(window(PciWindow::io))
            .chain(mmio().map(window(PciWindow::mmio))),
        io().map(kept(LEGACY_IO)).map(window(PciWindow::io)).chain(
            mmio()
                .map(kept((non_pci.base, non_pci.end())))
                .map(window(PciWindow::mmio)),
        ),
    )
    .ok()
}

/// `range` without `hole`, as up to two ranges.
fn cut((lo, hi): (u64, u64), (hole_lo, hole_hi): (u64, u64)) -> [(u64, u64); 2] {
    if hole_hi <= lo || hole_lo >= hi {
        return [(lo, hi), (0, 0)];
    }
    [(lo, hole_lo.max(lo)), (hole_hi.min(hi), hi)]
}
