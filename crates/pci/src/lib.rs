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

/// Maximum number of allocation windows supplied by one PCI root bridge.
pub const MAX_PCI_ROOT_WINDOWS: usize = 8;

/// Allocation-window snapshot supplied by a PCI root provider.
pub type PciRootWindows = heapless::Vec<PciWindow, MAX_PCI_ROOT_WINDOWS>;

/// Error while describing a PCI root bridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PciRootError {
    /// The root topology or a resource window is invalid.
    InvalidConfig,
    /// The provider supplied more windows than the bounded snapshot can hold.
    TooManyWindows,
}

/// Runtime provider for PCI root identity and allocatable address windows.
///
/// Root identity is fixed, while windows may be derived after memory discovery
/// from chipset registers, the memory map, and reserved firmware regions.
pub trait PciRootProvider: Send + Sync {
    /// Return fixed root-bridge identity and ECAM topology.
    fn root_info(&self) -> PciRootInfo;

    /// Compute the windows currently available for PCI resource allocation.
    fn resource_windows(&self) -> Result<PciRootWindows, PciRootError>;
}

/// A PCI root bridge: its identity and the windows its allocator may use.
///
/// This is the one description of a root bridge. A chipset or SoC builds it
/// from what decides its windows (TOLUD and the memory map, a device tree,
/// the Data Fabric's routes); the allocator ([`PciEcam::from_provider`]),
/// the ACPI host bridge and the payload read it.
#[derive(Debug, Clone)]
pub struct PciRoot {
    pub info: PciRootInfo,
    pub windows: PciRootWindows,
}

impl PciRoot {
    /// The root `info` with the non-empty `windows`.
    pub fn new(
        info: PciRootInfo,
        windows: impl IntoIterator<Item = PciWindow>,
    ) -> Result<Self, PciRootError> {
        if info.bus_end < info.bus_start {
            return Err(PciRootError::InvalidConfig);
        }
        let mut kept = PciRootWindows::new();
        for window in windows.into_iter().filter(|window| window.size != 0) {
            kept.push(window)
                .map_err(|_| PciRootError::TooManyWindows)?;
        }
        Ok(Self {
            info,
            windows: kept,
        })
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

impl PciRootProvider for PciRoot {
    fn root_info(&self) -> PciRootInfo {
        self.info
    }

    fn resource_windows(&self) -> Result<PciRootWindows, PciRootError> {
        Ok(self.windows.clone())
    }
}
