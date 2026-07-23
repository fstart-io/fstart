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
pub mod window;

pub use config::*;
pub use ecam::EcamDevice;
pub use ecam_host::{PciEcam, PciEcamConfig, PciEcamError};
pub use overlay::{PciType0Config, PciType1Config};
pub use pci_types::{capability, ConfigRegionAccess, HeaderType, PciAddress, PciHeader};
pub use window::{PciWindow, PciWindowKind};

#[doc(hidden)]
pub mod fstart_core {
    pub use fstart_core::*;
}

/// Fixed identity and config-space topology of one PCI root bridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PciRootInfo {
    /// PCI segment / domain number.
    pub segment: u16,
    /// ECAM base address.
    pub ecam_base: u64,
    /// First bus number owned by this root bridge.
    pub bus_start: u8,
    /// Last bus number owned by this root bridge (inclusive).
    pub bus_end: u8,
}

impl PciRootInfo {
    /// ECAM region size derived from the inclusive bus range.
    #[must_use]
    pub const fn ecam_size(&self) -> u64 {
        if self.bus_end < self.bus_start {
            return 0;
        }
        (self.bus_end as u64 - self.bus_start as u64 + 1) * 1024 * 1024
    }
}

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
