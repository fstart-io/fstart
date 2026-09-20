#![no_std]
#![recursion_limit = "256"]
#![allow(clippy::modulo_one)]

extern crate alloc;

#[cfg(feature = "acpi")]
mod cpu;

pub mod generic;
pub mod gm965;
pub mod gmch;
pub mod i945;
pub mod ich7;
pub mod ich8;
pub mod igd;
pub mod pineview;
pub use fstart_arch::x86::cpu::intel::microcode;
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
    /// Mainstage chipset init after the bus scan and before the southbridge
    /// devices (coreboot `northbridge_init`): DMI/egress, PM, IOMMU windows.
    fn post_dram_init(&mut self) -> Result<(), fstart_core::services::ServiceError> {
        Ok(())
    }
    fn stage_local_init(&mut self) -> Result<(), fstart_core::services::ServiceError>;

    /// Chipset work that needs *verified* boot media: the graphics OpRegion and
    /// the modeset, which embed and read the VBT. Runs after the
    /// `verify_boot_media` phase; platforms without such work keep the default.
    fn post_verify_init(&mut self) -> Result<(), fstart_core::services::ServiceError> {
        Ok(())
    }

    /// Caches policy derived from the detected memory map.
    fn memory_detected(&mut self, _e820: &fstart_core::services::memory_detect::E820State) {}

    /// Framebuffer programmed during [`Self::stage_local_init`], if the board
    /// asked for display bring-up. The platform hands this to the payload.
    fn framebuffer_info(&self) -> Option<fstart_core::services::FramebufferInfo> {
        None
    }
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
    /// Chipset-owned PCI BARs that generic resource allocation must preserve.
    fn fixed_pci_bars(&self) -> fstart_pci::PciFixedBars {
        fstart_pci::PciFixedBars::new()
    }
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

    /// Reset the system through the generic x86 CF9 port. Southbridges with
    /// sticky CF9 side state (e.g. ICH7's ETR3) override this to clear it.
    /// Used on unrecoverable S3-resume paths (no wake vector, invalid stage
    /// cache); never returns.
    fn system_reset(&self, hard: bool) -> ! {
        fstart_arch::x86_64::system_reset(hard)
    }
}
