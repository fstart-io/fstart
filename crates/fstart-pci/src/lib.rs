//! Shared PCI vocabulary, constants, and config-space helpers.
//!
//! This crate is deliberately backend-neutral: it defines PCI address types,
//! resource windows, standard register offsets, bitfields, and generic config
//! access traits without depending on any service or ECAM implementation.

#![no_std]

pub mod addr;
pub mod capability;
pub mod config;
pub mod ecam;
pub mod ecam_host;
pub mod overlay;
pub mod window;

pub use addr::{PciBdf, PciSbdf};
pub use capability::find_capability;
pub use config::*;
pub use ecam::EcamDevice;
pub use ecam_host::{PciEcam, PciEcamConfig};
pub use overlay::{PciType0Config, PciType1Config};
pub use window::{PciWindow, PciWindowKind};

#[doc(hidden)]
pub mod fstart_core {
    pub use fstart_core::*;
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

/// A PCI root bus (host bridge) that owns a segment's config-space and
/// MMIO/IO address windows.
pub trait PciRootBus: Send + Sync {
    /// Enumerate the root bus and allocate PCI resources.
    fn init_bus(&mut self) -> Result<(), fstart_core::services::ServiceError> {
        Ok(())
    }

    /// Read a 32-bit PCI configuration register via the root bus backend.
    fn config_read32(
        &self,
        addr: PciBdf,
        reg: u16,
    ) -> Result<u32, fstart_core::services::ServiceError>;

    /// Write a 32-bit PCI configuration register via the root bus backend.
    fn config_write32(
        &self,
        addr: PciBdf,
        reg: u16,
        val: u32,
    ) -> Result<(), fstart_core::services::ServiceError>;

    /// Read a 16-bit PCI configuration register (derived from `config_read32`).
    fn config_read16(
        &self,
        addr: PciBdf,
        reg: u16,
    ) -> Result<u16, fstart_core::services::ServiceError> {
        let val = self.config_read32(addr, reg & !0x3)?;
        let shift = ((reg & 0x2) * 8) as u32;
        Ok(((val >> shift) & 0xFFFF) as u16)
    }

    /// Write a 16-bit PCI configuration register (read-modify-write via `config_read32`).
    fn config_write16(
        &self,
        addr: PciBdf,
        reg: u16,
        val: u16,
    ) -> Result<(), fstart_core::services::ServiceError> {
        let aligned = reg & !0x3;
        let mut dword = self.config_read32(addr, aligned)?;
        let shift = ((reg & 0x2) * 8) as u32;
        dword &= !(0xFFFF << shift);
        dword |= (val as u32) << shift;
        self.config_write32(addr, aligned, dword)
    }

    /// Read an 8-bit PCI configuration register (derived from `config_read32`).
    fn config_read8(
        &self,
        addr: PciBdf,
        reg: u16,
    ) -> Result<u8, fstart_core::services::ServiceError> {
        let val = self.config_read32(addr, reg & !0x3)?;
        let shift = ((reg & 0x3) * 8) as u32;
        Ok(((val >> shift) & 0xFF) as u8)
    }

    /// Write an 8-bit PCI configuration register (read-modify-write via `config_read32`).
    fn config_write8(
        &self,
        addr: PciBdf,
        reg: u16,
        val: u8,
    ) -> Result<(), fstart_core::services::ServiceError> {
        let aligned = reg & !0x3;
        let mut dword = self.config_read32(addr, aligned)?;
        let shift = ((reg & 0x3) * 8) as u32;
        dword &= !(0xFF << shift);
        dword |= (val as u32) << shift;
        self.config_write32(addr, aligned, dword)
    }

    /// PCI segment / domain number. Defaults to 0 (single-segment systems).
    fn segment(&self) -> u16 {
        0
    }

    /// ECAM base address (for consumers that need raw MMIO access).
    fn ecam_base(&self) -> u64;

    /// ECAM region size in bytes.
    fn ecam_size(&self) -> u64;

    /// First bus number owned by this root bridge.
    fn bus_start(&self) -> u8;

    /// Last bus number owned by this root bridge (inclusive).
    fn bus_end(&self) -> u8;

    /// Number of discovered devices after `init()`.
    fn device_count(&self) -> usize;

    /// Address windows decoded by this root bridge.
    fn windows(&self) -> &[PciWindow];
}
