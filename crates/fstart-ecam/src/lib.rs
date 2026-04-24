//! Global ECAM PCI config space access.
//!
//! Call [`init`] once after programming PCIEXBAR, then create
//! [`PciDevBdf`] handles to access individual devices:
//!
//! ```ignore
//! fstart_ecam::init(0xE000_0000);
//! let lpc = fstart_ecam::PciDevBdf::new(0, 0x1f, 0);
//! let rev = lpc.read8(0x08);
//! lpc.write16(0x52, (1 << 8) | (3 << 4));
//! ```

#![no_std]

use core::sync::atomic::{AtomicUsize, Ordering};

static BASE: AtomicUsize = AtomicUsize::new(0);

/// Set the ECAM base address. Call exactly once after the CF8/CFC
/// write that programs PCIEXBAR.
pub fn init(base: usize) {
    // Mask off low 20 bits — callers may pass the raw PCIEXBAR value
    // which includes enable/size bits.  ECAM addresses are 1 MiB-aligned.
    BASE.store(base & !0xF_FFFF, Ordering::Release);
}

/// Return the current ECAM base (0 if uninitialised).
#[inline]
pub fn base() -> usize {
    BASE.load(Ordering::Acquire)
}

/// A PCI device address (bus/device/function) bound to the global ECAM
/// region.
///
/// Create with [`PciDevBdf::new`], then use the read/write methods to
/// access the device's PCI configuration registers without repeating the
/// BDF on every call.
///
/// ```ignore
/// let lpc = PciDevBdf::new(0, 0x1f, 0);
/// let rev = lpc.read8(0x08);
/// lpc.write16(0x52, (1 << 8) | (3 << 4));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PciDevBdf {
    bus: u8,
    dev: u8,
    func: u8,
}

impl PciDevBdf {
    /// Create a new PCI device handle for the given bus/device/function.
    #[inline]
    pub const fn new(bus: u8, dev: u8, func: u8) -> Self {
        Self { bus, dev, func }
    }

    /// Return the bus number.
    #[inline]
    pub const fn bus(&self) -> u8 {
        self.bus
    }

    /// Return the device number.
    #[inline]
    pub const fn dev(&self) -> u8 {
        self.dev
    }

    /// Return the function number.
    #[inline]
    pub const fn func(&self) -> u8 {
        self.func
    }

    #[inline]
    fn addr(&self, reg: u16) -> usize {
        BASE.load(Ordering::Acquire)
            | ((self.bus as usize) << 20)
            | ((self.dev as usize) << 15)
            | ((self.func as usize) << 12)
            | ((reg as usize) & 0xFFF)
    }

    /// Read a 32-bit PCI config register.
    #[inline]
    pub fn read32(&self, reg: u16) -> u32 {
        // SAFETY: ECAM region is memory-mapped PCI config space.
        unsafe { fstart_mmio::read32(self.addr(reg) as *const u32) }
    }

    /// Write a 32-bit PCI config register.
    #[inline]
    pub fn write32(&self, reg: u16, val: u32) {
        // SAFETY: ECAM region is memory-mapped PCI config space.
        unsafe { fstart_mmio::write32(self.addr(reg) as *mut u32, val) }
    }

    /// Read a 16-bit PCI config register.
    #[inline]
    pub fn read16(&self, reg: u16) -> u16 {
        // SAFETY: ECAM region is memory-mapped PCI config space.
        unsafe { fstart_mmio::read16(self.addr(reg) as *const u16) }
    }

    /// Write a 16-bit PCI config register.
    #[inline]
    pub fn write16(&self, reg: u16, val: u16) {
        // SAFETY: ECAM region is memory-mapped PCI config space.
        unsafe { fstart_mmio::write16(self.addr(reg) as *mut u16, val) }
    }

    /// Read an 8-bit PCI config register.
    #[inline]
    pub fn read8(&self, reg: u16) -> u8 {
        // SAFETY: ECAM region is memory-mapped PCI config space.
        unsafe { fstart_mmio::read8(self.addr(reg) as *const u8) }
    }

    /// Write an 8-bit PCI config register.
    #[inline]
    pub fn write8(&self, reg: u16, val: u8) {
        // SAFETY: ECAM region is memory-mapped PCI config space.
        unsafe { fstart_mmio::write8(self.addr(reg) as *mut u8, val) }
    }

    /// Read-modify-write: `reg = (reg & mask) | set`.
    #[inline]
    pub fn modify32(&self, reg: u16, mask: u32, set: u32) {
        let v = self.read32(reg);
        self.write32(reg, (v & mask) | set);
    }

    /// OR bits into a 32-bit register.
    #[inline]
    pub fn or32(&self, reg: u16, bits: u32) {
        self.modify32(reg, !0, bits);
    }

    /// AND bits out of an 8-bit register.
    #[inline]
    pub fn and8(&self, reg: u16, mask: u8) {
        let v = self.read8(reg);
        self.write8(reg, v & mask);
    }

    /// OR bits into an 8-bit register.
    #[inline]
    pub fn or8(&self, reg: u16, bits: u8) {
        let v = self.read8(reg);
        self.write8(reg, v | bits);
    }

    /// OR bits into a 16-bit register.
    #[inline]
    pub fn or16(&self, reg: u16, bits: u16) {
        let v = self.read16(reg);
        self.write16(reg, v | bits);
    }

    /// AND mask a 16-bit register.
    #[inline]
    pub fn and16(&self, reg: u16, mask: u16) {
        let v = self.read16(reg);
        self.write16(reg, v & mask);
    }

    /// AND mask a 32-bit register.
    #[inline]
    pub fn and32(&self, reg: u16, mask: u32) {
        let v = self.read32(reg);
        self.write32(reg, v & mask);
    }

    /// AND-then-OR an 8-bit register.
    #[inline]
    pub fn and8_or8(&self, reg: u16, mask: u8, bits: u8) {
        let v = self.read8(reg);
        self.write8(reg, (v & mask) | bits);
    }
}
