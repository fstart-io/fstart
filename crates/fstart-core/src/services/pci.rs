//! PCI root bus service — configuration space access and resource allocation.
//!
//! Generic PCI address/window types and standard config-space constants live in
//! `fstart-pci`. This module owns only the service trait used by platform/root
//! bridge drivers.

use super::ServiceError;

pub use fstart_pci::*;

/// A PCI root bus (host bridge) that owns a segment's config-space and
/// MMIO/IO address windows.
///
/// Board/platform PCI steps perform bus enumeration and resource allocation.
/// The trait methods below allow post-init queries and raw config-space access
/// for consumers that need it.
pub trait PciRootBus: Send + Sync {
    /// Enumerate the root bus and allocate PCI resources.
    fn init_bus(&mut self) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Read a 32-bit PCI configuration register via the root bus backend.
    fn config_read32(&self, addr: PciBdf, reg: u16) -> Result<u32, ServiceError>;

    /// Write a 32-bit PCI configuration register via the root bus backend.
    fn config_write32(&self, addr: PciBdf, reg: u16, val: u32) -> Result<(), ServiceError>;

    /// Read a 16-bit PCI configuration register (derived from `config_read32`).
    fn config_read16(&self, addr: PciBdf, reg: u16) -> Result<u16, ServiceError> {
        let val = self.config_read32(addr, reg & !0x3)?;
        let shift = ((reg & 0x2) * 8) as u32;
        Ok(((val >> shift) & 0xFFFF) as u16)
    }

    /// Write a 16-bit PCI configuration register (read-modify-write via `config_read32`).
    fn config_write16(&self, addr: PciBdf, reg: u16, val: u16) -> Result<(), ServiceError> {
        let aligned = reg & !0x3;
        let mut dword = self.config_read32(addr, aligned)?;
        let shift = ((reg & 0x2) * 8) as u32;
        dword &= !(0xFFFF << shift);
        dword |= (val as u32) << shift;
        self.config_write32(addr, aligned, dword)
    }

    /// Read an 8-bit PCI configuration register (derived from `config_read32`).
    fn config_read8(&self, addr: PciBdf, reg: u16) -> Result<u8, ServiceError> {
        let val = self.config_read32(addr, reg & !0x3)?;
        let shift = ((reg & 0x3) * 8) as u32;
        Ok(((val >> shift) & 0xFF) as u8)
    }

    /// Write an 8-bit PCI configuration register (read-modify-write via `config_read32`).
    fn config_write8(&self, addr: PciBdf, reg: u16, val: u8) -> Result<(), ServiceError> {
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
