//! Global ECAM PCI config space access.
//!
//! Call [`init`] once after programming PCIEXBAR, then create [`EcamDevice`]
//! handles to access individual devices.

use core::sync::atomic::{AtomicUsize, Ordering};

use crate::{ConfigRegionAccess, PciAddress};

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
    address: PciAddress,
}

impl EcamDevice {
    /// Create a new ECAM device handle for a PCI function address.
    #[inline]
    pub fn new_address(address: PciAddress) -> Self {
        Self { address }
    }

    /// Create a new segment-zero ECAM device handle.
    #[inline]
    pub fn new(bus: u8, device: u8, function: u8) -> Self {
        Self::new_address(PciAddress::new(0, bus, device, function))
    }

    /// Return the PCI function address.
    #[inline]
    pub fn address(&self) -> PciAddress {
        self.address
    }

    /// Return the bus number.
    #[inline]
    pub fn bus(&self) -> u8 {
        self.address.bus()
    }

    /// Return the device number.
    #[inline]
    pub fn dev(&self) -> u8 {
        self.address.device()
    }

    /// Return the function number.
    #[inline]
    pub fn func(&self) -> u8 {
        self.address.function()
    }

    #[inline]
    fn addr(&self, reg: u16) -> usize {
        BASE.load(Ordering::Acquire)
            | ((self.address.bus() as usize) << 20)
            | ((self.address.device() as usize) << 15)
            | ((self.address.function() as usize) << 12)
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
        unsafe { fstart_core::mmio::read32(self.addr(reg) as *const u32) }
    }

    /// Write a 32-bit PCI config register.
    #[inline]
    pub fn write32(&self, reg: u16, val: u32) {
        // SAFETY: ECAM region is memory-mapped PCI config space.
        unsafe { fstart_core::mmio::write32(self.addr(reg) as *mut u32, val) }
    }

    /// Read a 16-bit PCI config register.
    #[inline]
    pub fn read16(&self, reg: u16) -> u16 {
        // SAFETY: ECAM region is memory-mapped PCI config space.
        unsafe { fstart_core::mmio::read16(self.addr(reg) as *const u16) }
    }

    /// Write a 16-bit PCI config register.
    #[inline]
    pub fn write16(&self, reg: u16, val: u16) {
        // SAFETY: ECAM region is memory-mapped PCI config space.
        unsafe { fstart_core::mmio::write16(self.addr(reg) as *mut u16, val) }
    }

    /// Read an 8-bit PCI config register.
    #[inline]
    pub fn read8(&self, reg: u16) -> u8 {
        // SAFETY: ECAM region is memory-mapped PCI config space.
        unsafe { fstart_core::mmio::read8(self.addr(reg) as *const u8) }
    }

    /// Write an 8-bit PCI config register.
    #[inline]
    pub fn write8(&self, reg: u16, val: u8) {
        // SAFETY: ECAM region is memory-mapped PCI config space.
        unsafe { fstart_core::mmio::write8(self.addr(reg) as *mut u8, val) }
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

#[allow(unused_unsafe)]
impl ConfigRegionAccess for EcamDevice {
    unsafe fn read(&self, address: PciAddress, offset: u16) -> u32 {
        // SAFETY: the caller guarantees that the PCI address and offset are valid.
        unsafe { Self::new_address(address).read32(offset) }
    }

    unsafe fn write(&self, address: PciAddress, offset: u16, value: u32) {
        // SAFETY: the caller guarantees that the PCI address and offset are valid.
        unsafe { Self::new_address(address).write32(offset, value) }
    }
}
