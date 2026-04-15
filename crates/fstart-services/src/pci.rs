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
//!
//! # Resource windows
//!
//! A root bridge decodes one or more address windows on behalf of its
//! children.  Each window is either memory-mapped (MMIO) or I/O port
//! space, described by [`PciWindow`].  Whether a memory window is
//! reachable via 32-bit or 64-bit BARs is derived from its base and
//! size — no separate "32-bit" vs "64-bit" distinction is needed.
//!
//! The [`PciRootBus::windows`] method returns all windows as a slice,
//! so platforms with multiple MMIO ranges (e.g., separate prefetchable
//! and non-prefetchable regions, or below- and above-4 GiB ranges) can
//! describe their topology faithfully.

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

/// Kind of address space a [`PciWindow`] decodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PciWindowKind {
    /// Memory-mapped I/O.  Whether 32-bit or 64-bit BARs can target this
    /// window is determined by the address range: if `base + size <= 4 GiB`,
    /// 32-bit BARs fit; otherwise only 64-bit BARs can reach it.
    Mmio,
    /// I/O port space.  On architectures without native I/O ports
    /// (AArch64, RISC-V), the host bridge maps PCI I/O into a memory
    /// window — `base` is that MMIO address.  On x86, `base` is the
    /// first I/O port number.
    Io,
}

/// One address window decoded by a PCI root bridge.
///
/// A root bridge may decode several windows: low MMIO (below 4 GiB),
/// high MMIO (above 4 GiB), I/O ports, etc.  The allocator assigns
/// BARs from these windows during enumeration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PciWindow {
    /// Kind of address space (memory or I/O).
    pub kind: PciWindowKind,
    /// Base physical address (MMIO) or I/O port address.
    pub base: u64,
    /// Size of the window in bytes.
    pub size: u64,
    /// Whether this memory window supports prefetchable transactions.
    ///
    /// Only meaningful for [`PciWindowKind::Mmio`] windows.  ACPI `_CRS`
    /// resource descriptors and PCI bridge forwarding registers
    /// distinguish prefetchable from non-prefetchable memory.
    pub prefetchable: bool,
}

impl PciWindow {
    /// Exclusive end address (`base + size`), saturating at `u64::MAX`.
    pub const fn end(&self) -> u64 {
        self.base.saturating_add(self.size)
    }

    /// Whether a 32-bit BAR can target this window.
    ///
    /// True when the entire window fits within the low 4 GiB.
    pub const fn is_below_4g(&self) -> bool {
        self.base.saturating_add(self.size) <= 0x1_0000_0000
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

    /// Address windows decoded by this root bridge.
    ///
    /// Returns all MMIO and I/O windows that the root bridge forwards to
    /// PCI devices.  The allocator assigns BARs from these windows during
    /// enumeration.  Consumers (ACPI table generators, downstream PCI
    /// stacks) use these to learn the platform topology.
    ///
    /// A typical ECAM host bridge returns 2–3 windows: low MMIO
    /// (below 4 GiB), high MMIO (above 4 GiB), and I/O ports.
    fn windows(&self) -> &[PciWindow];
}
