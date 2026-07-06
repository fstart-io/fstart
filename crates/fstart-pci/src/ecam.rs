//! Global ECAM PCI config space access.
//!
//! Call [`init`] once after programming PCIEXBAR, then create [`EcamDevice`]
//! handles to access individual devices.

use core::convert::Infallible;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::{PciBdf, PciConfigAccess};

static BASE: AtomicUsize = AtomicUsize::new(0);

/// Set the ECAM base address. Call exactly once after the CF8/CFC
/// write that programs PCIEXBAR.
pub fn init(base: usize) {
    // Mask off low 20 bits — callers may pass the raw PCIEXBAR value
    // which includes enable/size bits. ECAM addresses are 1 MiB-aligned.
    BASE.store(base & !0xF_FFFF, Ordering::Release);
}

/// Return the current ECAM base (0 if uninitialised).
#[inline]
pub fn base() -> usize {
    BASE.load(Ordering::Acquire)
}

/// A PCI device handle bound to the global ECAM region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EcamDevice {
    bdf: PciBdf,
}

impl EcamDevice {
    /// Create a new ECAM device handle for a segment-local BDF.
    #[inline]
    pub const fn new_bdf(bdf: PciBdf) -> Self {
        Self { bdf }
    }

    /// Create a new ECAM device handle for the given bus/device/function.
    #[inline]
    pub const fn new(bus: u8, dev: u8, func: u8) -> Self {
        Self {
            bdf: PciBdf::new(bus, dev, func),
        }
    }

    /// Return the segment-local BDF.
    #[inline]
    pub const fn bdf(&self) -> PciBdf {
        self.bdf
    }

    /// Return the bus number.
    #[inline]
    pub const fn bus(&self) -> u8 {
        self.bdf.bus
    }

    /// Return the device number.
    #[inline]
    pub const fn dev(&self) -> u8 {
        self.bdf.dev
    }

    /// Return the function number.
    #[inline]
    pub const fn func(&self) -> u8 {
        self.bdf.func
    }

    #[inline]
    fn addr(&self, reg: u16) -> usize {
        BASE.load(Ordering::Acquire)
            | ((self.bdf.bus as usize) << 20)
            | ((self.bdf.dev as usize) << 15)
            | ((self.bdf.func as usize) << 12)
            | ((reg as usize) & 0xFFF)
    }

    /// Return a typed config-space overlay for this device.
    ///
    /// # Safety
    ///
    /// The caller must ensure that ECAM is initialized and stable, this BDF is
    /// present, `T` is a valid `register_structs!` overlay for PCI config
    /// space, and each accessed register is safe to manipulate through typed
    /// volatile register methods. Do not use typed overlays for BAR sizing,
    /// write-1-to-clear status handling, capability traversal, or exact-write
    /// errata sequences unless the replacement has been proven equivalent.
    #[inline]
    pub unsafe fn regs<T>(&self) -> &'static T {
        // SAFETY: guaranteed by the caller of this unsafe function.
        unsafe { &*(self.addr(0) as *const T) }
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

impl PciConfigAccess for EcamDevice {
    type Error = Infallible;

    fn read8(&self, bdf: PciBdf, reg: u16) -> Result<u8, Self::Error> {
        Ok(Self::new_bdf(bdf).read8(reg))
    }

    fn read16(&self, bdf: PciBdf, reg: u16) -> Result<u16, Self::Error> {
        Ok(Self::new_bdf(bdf).read16(reg))
    }

    fn read32(&self, bdf: PciBdf, reg: u16) -> Result<u32, Self::Error> {
        Ok(Self::new_bdf(bdf).read32(reg))
    }

    fn write8(&self, bdf: PciBdf, reg: u16, val: u8) -> Result<(), Self::Error> {
        Self::new_bdf(bdf).write8(reg, val);
        Ok(())
    }

    fn write16(&self, bdf: PciBdf, reg: u16, val: u16) -> Result<(), Self::Error> {
        Self::new_bdf(bdf).write16(reg, val);
        Ok(())
    }

    fn write32(&self, bdf: PciBdf, reg: u16, val: u32) -> Result<(), Self::Error> {
        Self::new_bdf(bdf).write32(reg, val);
        Ok(())
    }
}
