//! Global ECAM PCI config space access.
//!
//! Call [`init`] once after programming PCIEXBAR, then create [`EcamDevice`]
//! handles to access individual devices.

use core::sync::atomic::{AtomicUsize, Ordering};

use crate::{ConfigRegionAccess, PciAddress};

static BASE: AtomicUsize = AtomicUsize::new(0);

pub(crate) const FUNCTION_CONFIG_BYTES: usize = 0x1000;

/// ECAM function offset from the segment's bus-zero mapping.
#[inline]
pub(crate) fn function_offset(address: PciAddress) -> usize {
    ((address.bus() as usize) << 20)
        | ((address.device() as usize) << 15)
        | ((address.function() as usize) << 12)
}

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

/// A PCI device handle bound to a mapped function or the global ECAM region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EcamDevice {
    address: PciAddress,
    config_base: Option<usize>,
}

impl EcamDevice {
    /// Create a new ECAM device handle for a PCI function address.
    #[inline]
    pub fn new_address(address: PciAddress) -> Self {
        Self {
            address,
            config_base: None,
        }
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

    /// Return the PCI vendor ID.
    #[inline]
    pub fn vendor_id(&self) -> u16 {
        self.read16(0x00)
    }

    /// Return the PCI device ID.
    #[inline]
    pub fn device_id(&self) -> u16 {
        self.read16(0x02)
    }

    /// Whether this PCI function is present.
    #[inline]
    pub fn is_present(&self) -> bool {
        self.vendor_id() != 0xffff
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

    /// Bind a function already validated by its owning ECAM region.
    pub(crate) fn at_config_base(address: PciAddress, config_base: usize) -> Self {
        Self {
            address,
            config_base: Some(config_base),
        }
    }

    fn register_address(&self, reg: u16, width: usize) -> Option<usize> {
        let offset = usize::from(reg);
        if offset + width > FUNCTION_CONFIG_BYTES || offset % width != 0 {
            return None;
        }
        let base = match self.config_base {
            Some(base) => base,
            None => {
                let base = BASE.load(Ordering::Acquire);
                if base == 0 || self.address.segment() != 0 {
                    return None;
                }
                base.checked_add(function_offset(self.address))?
            }
        };
        base.checked_add(offset)
    }

    fn addr(&self, reg: u16, width: usize) -> usize {
        self.register_address(reg, width)
            .expect("invalid or unmapped ECAM register")
    }

    /// Return a typed config-space overlay for this device.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the bound mapping (or global ECAM) is stable, this BDF is
    /// present, `T` is a valid `register_structs!` overlay for PCI config
    /// space, and each accessed register is safe to manipulate through typed
    /// volatile register methods. Do not use typed overlays for BAR sizing,
    /// write-1-to-clear status handling, capability traversal, or exact-write
    /// errata sequences unless the replacement has been proven equivalent.
    #[inline]
    pub unsafe fn regs<T>(&self) -> &'static T {
        // SAFETY: guaranteed by the caller of this unsafe function.
        unsafe { &*(self.addr(0, 1) as *const T) }
    }

    /// Read a 32-bit PCI config register.
    #[inline]
    pub fn read32(&self, reg: u16) -> u32 {
        self.try_read32(reg)
            .expect("invalid or unmapped ECAM dword")
    }

    /// Write a 32-bit PCI config register.
    #[inline]
    pub fn write32(&self, reg: u16, val: u32) {
        self.try_write32(reg, val)
            .expect("invalid or unmapped ECAM dword");
    }

    /// Read a 16-bit PCI config register.
    #[inline]
    pub fn read16(&self, reg: u16) -> u16 {
        self.try_read16(reg).expect("invalid or unmapped ECAM word")
    }

    /// Write a 16-bit PCI config register.
    #[inline]
    pub fn write16(&self, reg: u16, val: u16) {
        self.try_write16(reg, val)
            .expect("invalid or unmapped ECAM word");
    }

    /// Read an 8-bit PCI config register.
    #[inline]
    pub fn read8(&self, reg: u16) -> u8 {
        self.try_read8(reg).expect("invalid or unmapped ECAM byte")
    }

    /// Write an 8-bit PCI config register.
    #[inline]
    pub fn write8(&self, reg: u16, val: u8) {
        self.try_write8(reg, val)
            .expect("invalid or unmapped ECAM byte");
    }

    /// Fallible accesses distinguish invalid/unmapped registers from hardware
    /// returning all ones for an absent function. Each uses its exact width.
    pub fn try_read8(&self, reg: u16) -> Option<u8> {
        let addr = self.register_address(reg, 1)?;
        // SAFETY: the owning region validated this mapped function.
        Some(unsafe { fstart_core::mmio::read8(addr as *const u8) })
    }

    pub fn try_read16(&self, reg: u16) -> Option<u16> {
        let addr = self.register_address(reg, 2)?;
        // SAFETY: mapped and aligned register within this function.
        Some(unsafe { fstart_core::mmio::read16(addr as *const u16) })
    }

    pub fn try_read32(&self, reg: u16) -> Option<u32> {
        let addr = self.register_address(reg, 4)?;
        // SAFETY: mapped and aligned register within this function.
        Some(unsafe { fstart_core::mmio::read32(addr as *const u32) })
    }

    pub fn try_write8(&self, reg: u16, val: u8) -> Option<()> {
        let addr = self.register_address(reg, 1)?;
        // SAFETY: the owning region validated this mapped function.
        unsafe { fstart_core::mmio::write8(addr as *mut u8, val) };
        Some(())
    }

    pub fn try_write16(&self, reg: u16, val: u16) -> Option<()> {
        let addr = self.register_address(reg, 2)?;
        // SAFETY: mapped and aligned register within this function.
        unsafe { fstart_core::mmio::write16(addr as *mut u16, val) };
        Some(())
    }

    pub fn try_write32(&self, reg: u16, val: u32) -> Option<()> {
        let addr = self.register_address(reg, 4)?;
        // SAFETY: mapped and aligned register within this function.
        unsafe { fstart_core::mmio::write32(addr as *mut u32, val) };
        Some(())
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

    /// Physical base of the memory BAR at `reg` as PCI enumeration assigned
    /// it, or `None` when the window is unassigned or an I/O BAR.
    ///
    /// Drivers that run after resource allocation must consume the
    /// allocator's assignment rather than re-programming a fixed address:
    /// the allocator has already laid the window out among every other
    /// device, and a driver-chosen base may alias a neighbour.
    pub fn memory_bar(&self, reg: u16) -> Option<u64> {
        const IO_SPACE: u32 = 1 << 0;
        const TYPE_64: u32 = 0b10 << 1;
        const TYPE_MASK: u32 = 0b11 << 1;
        let low = self.read32(reg);
        if low & IO_SPACE != 0 {
            return None;
        }
        let high = if low & TYPE_MASK == TYPE_64 {
            u64::from(self.read32(reg + 4)) << 32
        } else {
            0
        };
        let base = high | u64::from(low & !0xf);
        (base != 0).then_some(base)
    }
}

#[allow(unused_unsafe)]
impl ConfigRegionAccess for EcamDevice {
    unsafe fn read(&self, address: PciAddress, offset: u16) -> u32 {
        assert_eq!(
            address, self.address,
            "device accessor used for another PCI function"
        );
        self.read32(offset)
    }

    unsafe fn write(&self, address: PciAddress, offset: u16, value: u32) {
        assert_eq!(
            address, self.address,
            "device accessor used for another PCI function"
        );
        self.write32(offset, value)
    }
}
