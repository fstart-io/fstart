#![no_std]
#![recursion_limit = "256"]
#![allow(clippy::modulo_one)]

extern crate alloc;

pub mod cpu;
pub mod namespace;

pub mod generic;
pub mod gm45;
pub mod gm965;
pub mod gma;
pub mod gmch;
mod host_bridge;
pub mod i945;
pub mod ibexpeak;
pub mod ich7;
pub mod ich8;
pub mod ich9;
pub mod igd;
pub mod ironlake;
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

    /// Optional mapped-window bound for adapters with a fixed extent.
    fn mapped_size(self) -> Option<u32> {
        None
    }

    #[inline]
    fn register_address(self, off: u32, width: u32) -> usize {
        let end = off.checked_add(width).expect("MMIO offset overflow");
        assert!(self.mapped_size().is_none_or(|size| end <= size));
        let address = self
            .base()
            .checked_add(off as usize)
            .expect("MMIO address overflow");
        address
            .checked_add(width as usize - 1)
            .expect("MMIO access overflow");
        address
    }

    #[inline]
    fn read8(self, off: u32) -> u8 {
        // SAFETY: off is a register offset within this BAR.
        unsafe { fstart_core::mmio::read8(self.register_address(off, 1) as *const u8) }
    }

    #[inline]
    fn write8(self, off: u32, val: u8) {
        // SAFETY: off is a register offset within this BAR.
        unsafe { fstart_core::mmio::write8(self.register_address(off, 1) as *mut u8, val) }
    }

    #[inline]
    fn read16(self, off: u32) -> u16 {
        // SAFETY: off is a register offset within this BAR.
        let addr = self.register_address(off, 2) as *const u16;
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        if !addr.is_aligned() {
            // Some Intel register windows deliberately expose unaligned words.
            return unsafe { fstart_core::mmio::read16_unaligned(addr.cast()) };
        }
        unsafe { fstart_core::mmio::read16(addr) }
    }

    #[inline]
    fn write16(self, off: u32, val: u16) {
        // SAFETY: off is a register offset within this BAR.
        let addr = self.register_address(off, 2) as *mut u16;
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        if !addr.is_aligned() {
            unsafe { fstart_core::mmio::write16_unaligned(addr.cast(), val) };
            return;
        }
        unsafe { fstart_core::mmio::write16(addr, val) }
    }

    #[inline]
    fn read32(self, off: u32) -> u32 {
        // SAFETY: off is a register offset within this BAR.
        let addr = self.register_address(off, 4) as *const u32;
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        if !addr.is_aligned() {
            return unsafe { fstart_core::mmio::read32_unaligned(addr.cast()) };
        }
        unsafe { fstart_core::mmio::read32(addr) }
    }

    #[inline]
    fn write32(self, off: u32, val: u32) {
        // SAFETY: off is a register offset within this BAR.
        let addr = self.register_address(off, 4) as *mut u32;
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        if !addr.is_aligned() {
            unsafe { fstart_core::mmio::write32_unaligned(addr.cast(), val) };
            return;
        }
        unsafe { fstart_core::mmio::write32(addr, val) }
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

/// What a DIMM chipset's memory bring-up takes
/// ([`fstart_core::services::MemoryInit::Input`]): the SMBus its SPDs are
/// on and its training cache.
pub struct SpdTraining<'a> {
    pub smbus: &'a mut dyn fstart_core::services::SmBus,
    /// A training record to replay. The chipset validates it against the
    /// SPDs it reads first.
    pub cached: Option<&'a [u8]>,
    /// Where to capture this boot's training after the memory tests pass;
    /// empty when the board keeps no cache.
    pub capture: &'a mut [u8],
}

/// Intel northbridge contract consumed by Intel platform flows. Memory
/// bring-up is its [`MemoryInit`](fstart_core::services::MemoryInit): from
/// the SPDs and training cache, reporting the bytes captured, if any.
pub trait IntelNorthbridgeDriver:
    fstart_core::services::memory_detect::MemoryDetector
    + for<'a> fstart_core::services::MemoryInit<Input<'a> = SpdTraining<'a>, Output = Option<usize>>
    + Sized
{
    type Config: 'static;
    /// ACPI name of the PCI root below `\_SB`, coreboot's northbridge
    /// `acpi_name()` for its domain.
    const ACPI_PCI_ROOT_NAME: &'static str;
    /// ACPI name of the integrated graphics device on that root.
    const ACPI_IGD_NAME: &'static str;

    /// The PCI root bridge, once memory is detected: its identity, what the
    /// host bridge forwards and what that is for. The platform asks once;
    /// the allocator is built from the answer and keeps it, and PCI0's
    /// `_CRS`, `PDRC` and the payload's view of the root all come from the
    /// allocator's copy.
    fn pci_root(&self) -> Result<fstart_pci::PciRoot, fstart_pci::PciEcamError>;

    /// The user options the driver consumes (`fstart_options`): the
    /// configuration its knobs change, set before `pre_console_init` in
    /// every stage that runs the driver.
    type Options: Clone + 'static;
    const KNOBS: &'static [fstart_options::Knob<Self::Options>];
    /// The board's values of the options.
    fn options(&self) -> Self::Options;
    fn set_options(&mut self, options: Self::Options);
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
    /// Linked bootstrap DRAM windows as half-open physical address pairs.
    /// Chipsets requiring pre-publication probes override this hook.
    fn set_bootstrap_windows(
        &mut self,
        _postcar: (u64, u64),
        _mainstage: (u64, u64),
    ) -> Result<(), fstart_core::services::ServiceError> {
        Ok(())
    }
    /// Stable chipset/CPU/SPD-map and memory-policy identity, not boot status.
    fn training_identity(&self) -> Option<[u8; 32]> {
        None
    }
    /// Installed-DRAM inventory from the last successful DRAM init, for SMBIOS.
    fn memory_info(&self) -> Option<fstart_core::memory_info::MemoryInfo> {
        None
    }
    /// Advertise S3 only after both training persistence and retained stage
    /// storage have been established by the platform.
    fn set_s3_enabled(&mut self, _enabled: bool) {}
    /// Whether the family implements a retained-memory resume sequence.
    /// This capability enables hardware testing; it is not a validation claim.
    /// Training and retained-stage requirements remain platform policy.
    fn supports_s3_replay(&self) -> bool {
        false
    }
    /// Prepare retained DRAM for a clean reset; the southbridge then owns the
    /// actual reset-controller sequence, including any sticky CF9 state.
    fn prepare_resume_reset(&self) {}
    fn early_post_dram_init(&mut self) -> Result<(), fstart_core::services::ServiceError> {
        Ok(())
    }
    /// Complete post-DRAM PM/IGD programming after both DMI peers negotiated.
    fn finish_early_post_dram_init(&mut self) -> Result<(), fstart_core::services::ServiceError> {
        Ok(())
    }
    /// Mainstage chipset init after the bus scan and before the southbridge
    /// devices (coreboot `northbridge_init`), including DMA-remap windows.
    fn post_dram_init(&mut self) -> Result<(), fstart_core::services::ServiceError> {
        Ok(())
    }
    fn stage_local_init(&mut self) -> Result<(), fstart_core::services::ServiceError>;

    /// S3 resume in place of `stage_local_init` and `post_verify_init`:
    /// restore device state the suspended OS expects, without allocating or
    /// modesetting. `opregion` is the address the cold boot published.
    fn resume_init(
        &mut self,
        _opregion: Option<u32>,
    ) -> Result<(), fstart_core::services::ServiceError> {
        Ok(())
    }

    /// Chipset work that needs *verified* boot media: the graphics OpRegion and
    /// the modeset, which embed and read the VBT. Runs after the
    /// `verify_boot_media` phase; platforms without such work keep the default.
    fn post_verify_init(
        &mut self,
        _opregion: igd::OpRegionMemory<'_>,
    ) -> Result<(), fstart_core::services::ServiceError> {
        Ok(())
    }

    /// Caches policy derived from the detected memory map.
    fn memory_detected(&mut self, _e820: &fstart_core::services::memory_detect::E820State) {}

    /// Mainstage chip init before PCI enumeration (coreboot
    /// `chip_operations.init`), e.g. hiding functions the board disables.
    fn chip_init(&mut self) -> Result<(), fstart_core::services::ServiceError> {
        Ok(())
    }

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
    /// ACPI name of the LPC bridge, coreboot's `lpc_acpi_name()`.
    const ACPI_LPC_NAME: &'static str;
    /// The user options the driver consumes (`fstart_options`): the
    /// configuration its knobs change, set before `pre_console_init` in
    /// every stage that runs the driver.
    type Options: Clone + 'static;
    const KNOBS: &'static [fstart_options::Knob<Self::Options>];
    /// The board's values of the options.
    fn options(&self) -> Self::Options;
    fn set_options(&mut self, options: Self::Options);

    fn new_from_config(
        config: &'static Self::Config,
    ) -> Result<Self, fstart_core::services::ServiceError>;
    fn config(&self) -> &'static Self::Config;
    fn pre_console_init(&mut self) -> Result<(), fstart_core::services::ServiceError>;
    fn early_init(&mut self) -> Result<(), fstart_core::services::ServiceError>;
    /// Bootblock-only setup right before DRAM training (coreboot romstage
    /// `early_pch_init()`), which the DRAM mainstage must not repeat.
    fn pre_raminit_init(&mut self) -> Result<(), fstart_core::services::ServiceError> {
        Ok(())
    }
    /// Mainstage function enables before PCI enumeration (coreboot
    /// `enable_dev`/`.enable`), e.g. hiding the functions a board disables.
    fn chip_init(&mut self) -> Result<(), fstart_core::services::ServiceError> {
        Ok(())
    }
    /// Program a bus-0 function's subsystem ID; chipsets with
    /// write-protected subsystem registers unlock them here.
    fn program_subsystem_id(&self, device: fstart_pci::ecam::EcamDevice, id: u32) -> bool {
        device.set_subsystem(id)
    }
    /// Chipset-owned PCI BARs that generic resource allocation must preserve.
    fn fixed_pci_bars(&self) -> fstart_pci::PciFixedBars {
        fstart_pci::PciFixedBars::new()
    }
    /// Stable southbridge identity for the platform's training-cache policy.
    fn training_identity(&self) -> Option<[u8; 5]> {
        None
    }
    fn detect_s3_resume(&self) -> bool {
        false
    }
    fn set_s3_enabled(&mut self, _enabled: bool) {}
    /// Tell the mainstage driver that this boot resumes from S3, so it leaves
    /// state the suspended OS still owns (such as its RTC wake alarm) alone.
    fn set_resume(&mut self, _resume: bool) {}
    fn smbus_mut(&mut self) -> Option<&mut dyn fstart_core::services::SmBus> {
        None
    }
    /// Enable the southbridge DMI peer before northbridge negotiation.
    fn prepare_early_post_dram_init(&mut self) -> Result<(), fstart_core::services::ServiceError> {
        Ok(())
    }
    /// Poll the southbridge link after northbridge negotiation.
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
        fstart_arch::x86::boot::system_reset(hard)
    }
}

#[cfg(test)]
mod mmio_tests {
    use super::MmioBar;

    #[derive(Clone, Copy)]
    struct Window(usize);
    impl MmioBar for Window {
        fn base(self) -> usize {
            self.0
        }
        fn mapped_size(self) -> Option<u32> {
            Some(8)
        }
    }

    #[test]
    fn window_preserves_unaligned_widths_and_neighboring_bytes() {
        let mut registers = [0xa5a5_a5a5u32; 2];
        let bar = Window(registers.as_mut_ptr() as usize);
        bar.write16(1, 0x1234);
        bar.write32(4, 0x56789abc);
        assert_eq!(bar.read8(0), 0xa5);
        assert_eq!(bar.read16(1), 0x1234);
        assert_eq!(bar.read8(3), 0xa5);
        assert_eq!(bar.read32(4), 0x56789abc);
        assert_eq!(bar.register_address(7, 1), bar.base() + 7);
    }

    #[test]
    #[should_panic]
    fn window_rejects_access_crossing_its_end() {
        Window(0).register_address(5, 4);
    }

    #[test]
    #[should_panic]
    fn window_rejects_offset_overflow() {
        Window(0).register_address(u32::MAX, 4);
    }
}

#[cfg(test)]
mod option_tests {
    use super::{IntelNorthbridgeDriver, IntelSouthbridgeDriver};

    fn pair<N: IntelNorthbridgeDriver, S: IntelSouthbridgeDriver>() {
        let metas = fstart_log::option::KNOBS
            .iter()
            .map(|k| &k.meta)
            .chain(N::KNOBS.iter().map(|k| &k.meta))
            .chain(S::KNOBS.iter().map(|k| &k.meta));
        assert_eq!(fstart_options::duplicate_key(metas.clone()), None);
        assert_eq!(fstart_options::cmos::collision(metas), None);
    }

    #[test]
    fn every_chipset_pairing_has_distinct_option_keys() {
        pair::<crate::i945::IntelI945, crate::ich7::IntelIch7>();
        pair::<crate::pineview::IntelPineview, crate::ich7::IntelIch7>();
        pair::<crate::gm965::IntelGm965, crate::ich8::IntelIch8>();
        pair::<crate::gm45::IntelGm45, crate::ich9::IntelIch9>();
    }

    #[test]
    fn ironlake_and_ibex_peak_have_distinct_option_keys() {
        pair::<crate::ironlake::IntelIronlake, crate::ibexpeak::IntelIbexPeak>();
    }
}
