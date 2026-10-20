// SPDX-License-Identifier: MIT OR Apache-2.0
//! PCI root complex of a die: its bus range and the DF windows routed to it.
use super::layout::Region;
use fstart_driver_amd::{df::map, soc::Soc};
use fstart_pci::{
    ConfigRegionAccess, PciRootError, PciRootInfo, PciRootProvider, PciRootWindows, PciWindow,
    PciWindowKind,
};

/// Legacy LPC/FCH I/O stays below 0x1000; PCI gets the rest of the 64 KiB.
const PCI_IO: (u64, u64) = (0x1000, 0xf000);
/// The IOAPIC, HPET and FCH fixed MMIO and everything above.
const FIXED_MMIO: u64 = 0xfec0_0000;

pub struct Root {
    info: PciRootInfo,
    windows: PciRootWindows,
}

impl Root {
    /// The below-4G DF MMIO windows of `die`, minus the ECAM and the
    /// `reserved` MMIO (a non-PCI pool). Only 32-bit windows: the 64-bit DF
    /// window starts at 1 TiB, beyond what firmware maps, and the OS can
    /// still use it.
    pub fn new<S: Soc, P: ConfigRegionAccess + Copy>(
        die: &fstart_driver_amd::df::Die<S, P>,
        buses: core::ops::RangeInclusive<u8>,
        ecam: Region,
        reserved: Region,
    ) -> Self {
        let df0 = die.df();
        let pool = (reserved.base, reserved.end());
        let ecam_range = (ecam.base, ecam.end());
        let mut windows = PciRootWindows::new();
        let _ = windows.push(PciWindow {
            kind: PciWindowKind::Io,
            base: PCI_IO.0,
            size: PCI_IO.1,
            prefetchable: false,
        });
        for (base, size) in map::mmio_windows(&df0, die.ioms_fabric_id()) {
            let end = base + size;
            if end > 1 << 32 {
                continue;
            }
            let end = end.min(FIXED_MMIO);
            for (lo, hi) in [(base, end)]
                .into_iter()
                .flat_map(|r| cut(r, ecam_range))
                .flat_map(|r| cut(r, pool))
            {
                if hi > lo {
                    let _ = windows.push(PciWindow {
                        kind: PciWindowKind::Mmio,
                        base: lo,
                        size: hi - lo,
                        prefetchable: false,
                    });
                }
            }
        }
        Self {
            info: PciRootInfo {
                segment: 0,
                ecam_base: ecam.base,
                bus_start: *buses.start(),
                bus_end: *buses.end(),
            },
            windows,
        }
    }
}

/// `range` without `hole`, as up to two ranges.
fn cut((lo, hi): (u64, u64), (hole_lo, hole_hi): (u64, u64)) -> [(u64, u64); 2] {
    if hole_hi <= lo || hole_lo >= hi {
        return [(lo, hi), (0, 0)];
    }
    [(lo, hole_lo.max(lo)), (hole_hi.min(hi), hi)]
}

impl PciRootProvider for Root {
    fn root_info(&self) -> PciRootInfo {
        self.info
    }
    fn resource_windows(&self) -> Result<PciRootWindows, PciRootError> {
        Ok(self.windows.clone())
    }
}
