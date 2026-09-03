#![no_std]
#![recursion_limit = "256"]
#![allow(clippy::modulo_one)]

extern crate alloc;

#[cfg(feature = "acpi")]
mod cpu;

pub mod generic;
pub mod gm965;
pub mod i945;
pub mod ich7;
pub mod ich8;
pub mod pineview;
pub use fstart_arch::cpu_intel::microcode;
pub mod southbridge;

/// Boot condition selected before DRAM initialization.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BootPath {
    /// Ordinary power-on initialization.
    #[default]
    Normal,
    /// Warm-reset recovery path.
    WarmReset,
    /// ACPI S3 resume path.
    S3Resume,
}

/// Raw MMIO BAR accessor shared by northbridge BAR handles.
///
/// `MchBar`/`DmiBar`/`EpBar`/`Rcba` across the gm965, i945, and pineview
/// drivers are distinct types over a plain base address; this trait owns
/// the single copy of the read/write/set/clear suite so drivers only
/// declare the handle (`new`) and the impl line. All operations are
/// volatile MMIO with the same ordering as `fstart_core::mmio`.
pub trait MmioBar: Copy {
    /// MMIO base address of the BAR.
    fn base(self) -> usize;

    #[inline]
    fn read8(self, off: u32) -> u8 {
        // SAFETY: off is a register offset within this BAR.
        unsafe { fstart_core::mmio::read8((self.base() + off as usize) as *const u8) }
    }

    #[inline]
    fn write8(self, off: u32, val: u8) {
        // SAFETY: off is a register offset within this BAR.
        unsafe { fstart_core::mmio::write8((self.base() + off as usize) as *mut u8, val) }
    }

    #[inline]
    fn read16(self, off: u32) -> u16 {
        // SAFETY: off is a register offset within this BAR.
        unsafe { fstart_core::mmio::read16((self.base() + off as usize) as *const u16) }
    }

    #[inline]
    fn write16(self, off: u32, val: u16) {
        // SAFETY: off is a register offset within this BAR.
        unsafe { fstart_core::mmio::write16((self.base() + off as usize) as *mut u16, val) }
    }

    #[inline]
    fn read32(self, off: u32) -> u32 {
        // SAFETY: off is a register offset within this BAR.
        unsafe { fstart_core::mmio::read32((self.base() + off as usize) as *const u32) }
    }

    #[inline]
    fn write32(self, off: u32, val: u32) {
        // SAFETY: off is a register offset within this BAR.
        unsafe { fstart_core::mmio::write32((self.base() + off as usize) as *mut u32, val) }
    }

    #[inline]
    fn setbits8(self, off: u32, bits: u8) {
        self.write8(off, self.read8(off) | bits);
    }

    #[inline]
    fn clrbits8(self, off: u32, bits: u8) {
        self.write8(off, self.read8(off) & !bits);
    }

    #[inline]
    fn clrsetbits8(self, off: u32, clear: u8, set: u8) {
        self.write8(off, (self.read8(off) & !clear) | set);
    }

    #[inline]
    fn setbits16(self, off: u32, bits: u16) {
        self.write16(off, self.read16(off) | bits);
    }

    #[inline]
    fn clrbits16(self, off: u32, bits: u16) {
        self.write16(off, self.read16(off) & !bits);
    }

    #[inline]
    fn clrsetbits16(self, off: u32, clear: u16, set: u16) {
        self.write16(off, (self.read16(off) & !clear) | set);
    }

    #[inline]
    fn setbits32(self, off: u32, bits: u32) {
        self.write32(off, self.read32(off) | bits);
    }

    #[inline]
    fn clrbits32(self, off: u32, bits: u32) {
        self.write32(off, self.read32(off) & !bits);
    }

    #[inline]
    fn clrsetbits32(self, off: u32, clear: u32, set: u32) {
        self.write32(off, (self.read32(off) & !clear) | set);
    }

    /// Read-modify-write with keep-mask: `reg = (reg & mask) | set`.
    #[inline]
    fn modify32(self, off: u32, mask: u32, set: u32) {
        self.write32(off, (self.read32(off) & mask) | set);
    }
}

/// Intel northbridge contract consumed by Intel platform flows.
pub trait IntelNorthbridgeDriver:
    fstart_core::services::memory_detect::MemoryDetector
    + fstart_core::services::MemoryController
    + fstart_pci::PciRootProvider
    + Sized
{
    type Config: 'static;

    fn new_from_config(
        config: &'static Self::Config,
    ) -> Result<Self, fstart_core::services::ServiceError>;
    fn config(&self) -> &'static Self::Config;
    fn pre_console_init(&mut self) -> Result<(), fstart_core::services::ServiceError>;
    fn early_init(&mut self) -> Result<(), fstart_core::services::ServiceError>;
    fn detect_warm_reset(&self) -> bool {
        false
    }
    fn set_boot_path(&mut self, _boot_path: BootPath) {}
    fn dram_init_with_smbus(
        &mut self,
        _smbus: Option<&mut dyn fstart_core::services::SmBus>,
    ) -> Result<(), fstart_core::services::ServiceError> {
        self.dram_init()
    }
    fn early_post_dram_init(&mut self) -> Result<(), fstart_core::services::ServiceError> {
        Ok(())
    }
    fn stage_local_init(&mut self) -> Result<(), fstart_core::services::ServiceError>;

    /// Caches policy derived from the detected memory map.
    fn memory_detected(&mut self, _e820: &fstart_core::services::memory_detect::E820State) {}
}

/// ECAM base exposed by northbridge config.
pub trait IntelEcamConfig {
    fn ecam_base(&self) -> u64;
}

/// Intel southbridge contract consumed by Intel platform flows.
pub trait IntelSouthbridgeDriver: Sized {
    type Config: 'static;

    fn new_from_config(
        config: &'static Self::Config,
    ) -> Result<Self, fstart_core::services::ServiceError>;
    #[cfg(feature = "acpi")]
    fn config(&self) -> &'static Self::Config;
    fn pre_console_init(&mut self) -> Result<(), fstart_core::services::ServiceError>;
    fn early_init(&mut self) -> Result<(), fstart_core::services::ServiceError>;
    fn detect_s3_resume(&self) -> bool {
        false
    }
    fn smbus_mut(&mut self) -> Option<&mut dyn fstart_core::services::SmBus> {
        None
    }
    fn early_post_dram_init(&mut self) -> Result<(), fstart_core::services::ServiceError> {
        Ok(())
    }
    fn post_dram_init(&mut self) -> Result<(), fstart_core::services::ServiceError>;
    fn finalize_init(&mut self) -> Result<(), fstart_core::services::ServiceError>;
}

/// Lazily heap-allocate the IGD opregion buffer at mainstage.
///
/// A `static` buffer would land in every stage's `.bss` — including the
/// bootblock, whose CAR is as small as 32 KiB on Pineview. The opregion is
/// only initialized post-DRAM, so it lives on the mainstage heap instead.
pub(crate) fn igd_opregion_buf(size: usize) -> &'static mut [u8] {
    use core::sync::atomic::{AtomicUsize, Ordering};

    static PTR: AtomicUsize = AtomicUsize::new(0);
    let mut p = PTR.load(Ordering::Relaxed);
    if p == 0 {
        let layout = core::alloc::Layout::from_size_align(size, 4096).expect("opregion layout");
        // SAFETY: non-zero-sized layout; the allocation is never freed (the
        // OS reads it through ASLS for the machine's lifetime).
        p = unsafe { alloc::alloc::alloc_zeroed(layout) } as usize;
        assert!(p != 0, "IGD opregion allocation failed");
        PTR.store(p, Ordering::Relaxed);
    }
    // SAFETY: BSP-only initialization before ASLS handoff; 'static because
    // the allocation is never freed.
    unsafe { core::slice::from_raw_parts_mut(p as *mut u8, size) }
}
