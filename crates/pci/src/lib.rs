//! Shared PCI vocabulary, constants, and config-space helpers.
//!
//! This crate is deliberately backend-neutral: it re-exports the canonical
//! `pci_types` address/configuration API and adds fstart's resource windows,
//! standard register constants, typed overlays, and ECAM allocator.

#![no_std]

pub mod config;
pub mod ecam;
pub mod ecam_host;
pub mod overlay;
pub mod register;
pub mod window;

pub use config::*;
pub use ecam::EcamDevice;
pub use ecam_host::{
    MAX_PCI_FIXED_BARS, MAX_PCI_ROOTS, PciEcam, PciEcamError, PciFixedBar, PciFixedBarType,
    PciFixedBars,
};
pub use overlay::{PciType0Config, PciType1Config};
pub use pci_types::{ConfigRegionAccess, HeaderType, PciAddress, PciHeader, capability};
pub use register::{ConfigRegister, bdf, devfn, devfn_parts};
pub use window::{PciWindow, PciWindowKind};

#[doc(hidden)]
pub mod fstart_core {
    pub use fstart_core::*;
}

pub use fstart_core::description::PciRootInfo;

/// The most windows, and the most reservations, one PCI root bridge has.
pub const MAX_PCI_ROOT_WINDOWS: usize = 8;

/// The windows of one PCI root bridge.
type PciRootWindows = heapless::Vec<PciWindow, MAX_PCI_ROOT_WINDOWS>;

/// A PCI root bridge: its identity, the windows it forwards to PCI and the
/// parts of them the firmware keeps for itself.
///
/// This is the one description of a root bridge and the only way into the
/// allocator. A chipset or SoC builds it once from what decides its windows
/// (everything that is not DRAM on an Intel host bridge, a device tree, the
/// Data Fabric's routes). ACPI reports the windows; the allocator
/// ([`PciEcam::new`]) works in them outside the reserved parts.
#[derive(Debug, Clone)]
pub struct PciRoot {
    info: PciRootInfo,
    windows: PciRootWindows,
    reserved: PciRootWindows,
}

impl PciRoot {
    /// The root bridge `info` forwarding `windows` to PCI, of which the
    /// firmware keeps `reserved` (legacy I/O, fixed chipset MMIO, a non-PCI
    /// pool). Empty ranges are dropped.
    ///
    /// The ECAM window is never a window: the bridge consumes it itself.
    /// Every reservation lies in one window; a root with more windows or
    /// reservations than [`MAX_PCI_ROOT_WINDOWS`] is refused, not cut short.
    pub fn new(
        info: PciRootInfo,
        windows: impl IntoIterator<Item = PciWindow>,
        reserved: impl IntoIterator<Item = PciWindow>,
    ) -> Result<Self, PciEcamError> {
        if info.bus_end < info.bus_start {
            return Err(PciEcamError::ConfigError);
        }
        let collect = |ranges: &mut dyn Iterator<Item = PciWindow>| {
            let mut kept = PciRootWindows::new();
            for range in ranges.filter(|range| range.size != 0) {
                if range.base.checked_add(range.size).is_none() {
                    return Err(PciEcamError::ConfigError);
                }
                kept.push(range)
                    .map_err(|_| PciEcamError::ResourceRangeLimit)?;
            }
            Ok(kept)
        };
        let windows = collect(&mut windows.into_iter())?;
        let reserved = collect(&mut reserved.into_iter())?;
        let forwarded = |part: &PciWindow| {
            windows.iter().any(|window| {
                window.kind == part.kind && window.base <= part.base && part.end() <= window.end()
            })
        };
        if !reserved.iter().all(forwarded) {
            return Err(PciEcamError::ConfigError);
        }
        Ok(Self {
            info,
            windows,
            reserved,
        })
    }

    /// The root bridge's identity and place in the ECAM.
    pub fn info(&self) -> PciRootInfo {
        self.info
    }

    /// What the root bridge forwards to PCI.
    pub fn windows(&self) -> &[PciWindow] {
        &self.windows
    }

    /// What the allocator leaves alone within the windows.
    pub fn reserved(&self) -> &[PciWindow] {
        &self.reserved
    }

    /// The buses the root bridge owns.
    pub fn buses(&self) -> core::ops::RangeInclusive<u8> {
        self.info.bus_start..=self.info.bus_end
    }

    /// The I/O windows as port ranges.
    pub fn io(&self) -> impl Iterator<Item = core::ops::RangeInclusive<u16>> + '_ {
        self.ranges(PciWindowKind::Io)
            .map(|range| *range.start() as u16..=*range.end() as u16)
    }

    /// The MMIO windows as address ranges.
    pub fn mmio(&self) -> impl Iterator<Item = core::ops::RangeInclusive<u64>> + '_ {
        self.ranges(PciWindowKind::Mmio)
    }

    fn ranges(
        &self,
        kind: PciWindowKind,
    ) -> impl Iterator<Item = core::ops::RangeInclusive<u64>> + '_ {
        self.windows
            .iter()
            .filter(move |window| window.kind == kind)
            .map(|window| window.base..=window.end() - 1)
    }
}
