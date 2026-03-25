//! PCI root bus service — configuration space access and resource allocation.
//!
//! A PCI root bus (host bridge) provides ECAM-based config-space access to
//! devices on its segment.  Its `init()` method enumerates the bus tree,
//! sizes BARs, allocates resources from the MMIO/IO windows declared in its
//! config, programs hardware, and enables memory/IO decode.
//!
//! After `init()` returns, all BARs on the bus tree are programmed and the
//! devices are ready for use by downstream consumers (e.g., CrabEFI's own
//! PCI driver model which *reads* pre-allocated BARs).

use crate::ServiceError;

/// PCI address: bus / device / function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PciAddr {
    pub bus: u8,
    pub dev: u8,
    pub func: u8,
}

impl PciAddr {
    pub const fn new(bus: u8, dev: u8, func: u8) -> Self {
        Self { bus, dev, func }
    }
}

/// A PCI root bus (host bridge) that owns a segment's config-space and
/// MMIO/IO address windows.
///
/// The driver's `Device::init()` performs full bus enumeration and resource
/// allocation.  The trait methods below allow post-init queries and raw
/// config-space access for consumers that need it.
pub trait PciRootBus: Send + Sync {
    /// Read a 32-bit PCI configuration register via ECAM.
    fn config_read32(&self, addr: PciAddr, reg: u16) -> Result<u32, ServiceError>;

    /// Write a 32-bit PCI configuration register via ECAM.
    fn config_write32(&self, addr: PciAddr, reg: u16, val: u32) -> Result<(), ServiceError>;

    /// Read a 16-bit PCI configuration register (derived from `config_read32`).
    fn config_read16(&self, addr: PciAddr, reg: u16) -> Result<u16, ServiceError> {
        let val = self.config_read32(addr, reg & !0x3)?;
        let shift = ((reg & 0x2) * 8) as u32;
        Ok(((val >> shift) & 0xFFFF) as u16)
    }

    /// Write a 16-bit PCI configuration register (read-modify-write via `config_read32`).
    fn config_write16(&self, addr: PciAddr, reg: u16, val: u16) -> Result<(), ServiceError> {
        let aligned = reg & !0x3;
        let mut dword = self.config_read32(addr, aligned)?;
        let shift = ((reg & 0x2) * 8) as u32;
        dword &= !(0xFFFF << shift);
        dword |= (val as u32) << shift;
        self.config_write32(addr, aligned, dword)
    }

    /// Read an 8-bit PCI configuration register (derived from `config_read32`).
    fn config_read8(&self, addr: PciAddr, reg: u16) -> Result<u8, ServiceError> {
        let val = self.config_read32(addr, reg & !0x3)?;
        let shift = ((reg & 0x3) * 8) as u32;
        Ok(((val >> shift) & 0xFF) as u8)
    }

    /// Write an 8-bit PCI configuration register (read-modify-write via `config_read32`).
    fn config_write8(&self, addr: PciAddr, reg: u16, val: u8) -> Result<(), ServiceError> {
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
}
