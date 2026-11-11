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

    /// The windows the root bridge forwards to PCI: what the OS is told it
    /// may use. The ECAM window is not one of them.
    fn resource_windows(&self) -> Result<PciRootWindows, PciRootError>;

    /// The parts of those windows the firmware keeps for itself (legacy
    /// I/O, fixed chipset MMIO, a non-PCI pool): the allocator stays out of
    /// them, the root bridge still decodes them.
    fn reserved_windows(&self) -> Result<PciRootWindows, PciRootError> {
        Ok(PciRootWindows::new())
    }
}

/// A PCI root bridge: its identity, the windows it decodes and the parts of
/// them the firmware keeps for itself.
///
/// This is the one description of a root bridge. A chipset or SoC builds it
/// from what decides its windows (everything that is not DRAM on an Intel
/// host bridge, a device tree, the Data Fabric's routes). ACPI reports the
/// windows; the allocator ([`PciEcam::from_provider`]) works in them
/// outside the reserved parts.
#[derive(Debug, Clone)]
pub struct PciRoot {
    pub info: PciRootInfo,
    /// What the root bridge forwards to PCI. Never its ECAM window, which
    /// it consumes itself.
    pub windows: PciRootWindows,
    /// What the allocator must leave alone within the windows.
    pub reserved: PciRootWindows,
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
            reserved: PciRootWindows::new(),
        })
    }

    /// The same root with `reserved` kept out of the allocator's hands.
    pub fn reserve(
        mut self,
        reserved: impl IntoIterator<Item = PciWindow>,
    ) -> Result<Self, PciRootError> {
        for window in reserved.into_iter().filter(|window| window.size != 0) {
            self.reserved
                .push(window)
                .map_err(|_| PciRootError::TooManyWindows)?;
        }
        Ok(self)
    }

    /// The root bridge a provider describes, as data.
    pub fn of(provider: &(impl PciRootProvider + ?Sized)) -> Result<Self, PciRootError> {
        Self::new(provider.root_info(), provider.resource_windows()?)?
            .reserve(provider.reserved_windows()?)
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

    fn reserved_windows(&self) -> Result<PciRootWindows, PciRootError> {
        Ok(self.reserved.clone())
    }
}
