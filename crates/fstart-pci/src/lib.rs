//! Shared PCI vocabulary, constants, and config-space helpers.
//!
//! This crate is deliberately backend-neutral: it defines PCI address types,
//! resource windows, standard register offsets, bitfields, and generic config
//! access traits without depending on any service or ECAM implementation.

#![no_std]

pub mod addr;
pub mod capability;
pub mod config;
pub mod overlay;
pub mod window;

pub use addr::{PciBdf, PciSbdf};
pub use capability::find_capability;
pub use config::*;
pub use overlay::{PciType0Config, PciType1Config};
pub use window::{PciWindow, PciWindowKind};

#[doc(hidden)]
pub mod fstart_mmio {
    pub use fstart_mmio::*;
}

/// Backend-neutral PCI configuration-space access.
///
/// Implementations may use ECAM, I/O ports, firmware calls, or a root-bus
/// service. Addresses are segment-local [`PciBdf`] values; callers with more
/// than one segment should select the appropriate backend/root bus first.
pub trait PciConfigAccess {
    /// Backend-specific access error.
    type Error;

    /// Read an 8-bit PCI configuration register.
    fn read8(&self, bdf: PciBdf, reg: u16) -> Result<u8, Self::Error>;

    /// Read a 16-bit PCI configuration register.
    fn read16(&self, bdf: PciBdf, reg: u16) -> Result<u16, Self::Error>;

    /// Read a 32-bit PCI configuration register.
    fn read32(&self, bdf: PciBdf, reg: u16) -> Result<u32, Self::Error>;

    /// Write an 8-bit PCI configuration register.
    fn write8(&self, bdf: PciBdf, reg: u16, val: u8) -> Result<(), Self::Error>;

    /// Write a 16-bit PCI configuration register.
    fn write16(&self, bdf: PciBdf, reg: u16, val: u16) -> Result<(), Self::Error>;

    /// Write a 32-bit PCI configuration register.
    fn write32(&self, bdf: PciBdf, reg: u16, val: u32) -> Result<(), Self::Error>;
}
