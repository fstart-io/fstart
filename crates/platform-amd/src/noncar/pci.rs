// SPDX-License-Identifier: MIT OR Apache-2.0
//! The PCI root complex of a die as a PCI root bridge: the buses and windows
//! the DF routes to it. The allocator and the ACPI host bridge
//! of the die both take them from here.
use super::layout::Region;
use fstart_driver_amd::df::map::RootComplex;
use fstart_pci::{PciRoot, PciRootInfo, PciWindow, PciWindowKind};

/// Legacy LPC/FCH I/O stays below 0x1000; PCI gets the rest of the 64 KiB.
const PCI_IO: (u64, u64) = (0x1000, 0x1_0000);
/// The IOAPIC, HPET and FCH fixed MMIO and everything above.
const FIXED_MMIO: u64 = 0xfec0_0000;

/// A die's root complex as a PCI root bridge.
pub type Root = PciRoot;

/// The root bridge of the root complex `routes`: its buses in the ECAM
/// `ecam`, its I/O windows above the legacy I/O, its MMIO windows below
/// 4 GiB minus the ECAM and the `reserved` MMIO (a non-PCI pool), and its
/// MMIO windows above 4 GiB. The allocator fills the low windows first;
/// ACPI reports all of them. `None` if the buses are outside the ECAM.
pub fn root(routes: &RootComplex, ecam: Region, reserved: Region) -> Option<Root> {
    let last_bus = u8::try_from((ecam.size >> 20).checked_sub(1)?).ok()?;
    let buses = *routes.buses.start()..=(*routes.buses.end()).min(last_bus);
    if buses.is_empty() {
        return None;
    }
    let pool = (reserved.base, reserved.end());
    let ecam_range = (ecam.base, ecam.end());
    let io = routes.io.iter().map(|&(base, size)| {
        let window = (base.max(PCI_IO.0), (base + size).min(PCI_IO.1));
        (PciWindowKind::Io, window)
    });
    let mmio = routes
        .mmio
        .iter()
        .filter(|&&(base, size)| base + size <= 1 << 32)
        .flat_map(|&(base, size)| cut((base, (base + size).min(FIXED_MMIO)), ecam_range))
        .flat_map(|range| cut(range, pool))
        .map(|range| (PciWindowKind::Mmio, range));
    // What the DF routes to the die above 4 GiB, for the 64-bit prefetchable
    // BARs that do not fit below.
    let high = routes
        .mmio
        .iter()
        .filter(|&&(base, _)| base >= 1 << 32)
        .map(|&(base, size)| (PciWindowKind::Mmio, (base, base + size)));
    let windows = io
        .chain(mmio)
        .chain(high)
        .filter(|(_, (lo, hi))| hi > lo)
        .map(|(kind, (lo, hi))| PciWindow {
            kind,
            base: lo,
            size: hi - lo,
            prefetchable: lo >= 1 << 32,
        })
        .take(fstart_pci::MAX_PCI_ROOT_WINDOWS)
        .collect();
    Some(PciRoot {
        info: PciRootInfo {
            segment: 0,
            ecam_base: ecam.base,
            bus_start: *buses.start(),
            bus_end: *buses.end(),
        },
        windows,
    })
}

/// `range` without `hole`, as up to two ranges.
fn cut((lo, hi): (u64, u64), (hole_lo, hole_hi): (u64, u64)) -> [(u64, u64); 2] {
    if hole_hi <= lo || hole_lo >= hi {
        return [(lo, hi), (0, 0)];
    }
    [(lo, hole_lo.max(lo)), (hole_hi.min(hi), hi)]
}
