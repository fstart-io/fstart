// SPDX-License-Identifier: MIT OR Apache-2.0
//! PCI root complex of a die: its bus range and the DF windows routed to it
//! (with the Zeppelin non-PCI pool).
use fstart_driver_amd::zeppelin::{
    ECAM, ECAM_BUSES, fabric,
    topology::{Die, NONPCI_MMIO_SIZE},
};
use fstart_pci::{
    PciRootError, PciRootInfo, PciRootProvider, PciRootWindows, PciWindow, PciWindowKind,
    ecam::GlobalEcam,
};

/// Legacy LPC/FCH I/O stays below 0x1000; PCI gets the rest of the 64 KiB.
const PCI_IO: (u64, u64) = (0x1000, 0xf000);

pub struct Root {
    bus_start: u8,
    bus_end: u8,
    windows: PciRootWindows,
}

impl Root {
    /// The below-4G DF MMIO windows of `die`, minus the ECAM and its non-PCI
    /// pool. Only 32-bit windows: the 64-bit DF window starts at 1 TiB,
    /// beyond what firmware maps, and the OS can still use it.
    pub fn new(die: &Die<GlobalEcam>, bus_end: u8, nonpci: u32) -> Self {
        let mut df0 = die.df();
        let pool = (u64::from(nonpci), u64::from(nonpci) + u64::from(NONPCI_MMIO_SIZE));
        let ecam = (ECAM, ECAM + (u64::from(ECAM_BUSES) << 20));
        let mut windows = PciRootWindows::new();
        let _ = windows.push(PciWindow {
            kind: PciWindowKind::Io,
            base: PCI_IO.0,
            size: PCI_IO.1,
            prefetchable: false,
        });
        for (base, size) in fabric::mmio_windows(&mut df0, die.index()) {
            let end = base + size;
            if end > 1 << 32 {
                continue;
            }
            // Cut out the ECAM, the non-PCI pool and everything at and above
            // the IOAPIC/HPET/FCH fixed MMIO (0xfec00000).
            let end = end.min(0xfec0_0000);
            for (lo, hi) in [(base, end)]
                .into_iter()
                .flat_map(|r| cut(r, ecam))
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
            bus_start: die.bus(),
            bus_end,
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
        PciRootInfo {
            segment: 0,
            ecam_base: ECAM,
            bus_start: self.bus_start,
            bus_end: self.bus_end,
        }
    }
    fn resource_windows(&self) -> Result<PciRootWindows, PciRootError> {
        Ok(self.windows.clone())
    }
}
