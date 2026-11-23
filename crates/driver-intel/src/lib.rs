#![no_std]
#![recursion_limit = "256"]
#![allow(clippy::modulo_one)]

extern crate alloc;

pub mod cpu;
pub mod namespace;

pub mod baytrail;
pub mod generic;
pub mod gm45;
pub mod gm965;
pub mod gma;
pub mod gmch;
pub mod haswell;
mod host_bridge;
pub mod i945;
pub mod ibexpeak;
pub mod ich7;
pub mod ich8;
pub mod ich9;
pub mod igd;
pub mod ironlake;
pub mod lynxpoint;
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
    /// The board's values of the options, from its configuration alone:
    /// what the driver starts from, and what a build's option schema lists.
    fn board_options(config: &Self::Config) -> Self::Options;
    /// The values the driver runs with.
    fn options(&self) -> Self::Options;
    fn set_options(&mut self, options: Self::Options);
    fn new_from_config(
        config: &'static Self::Config,
    ) -> Result<Self, fstart_core::services::ServiceError>;
    fn config(&self) -> &'static Self::Config;
    fn pre_console_init(&mut self) -> Result<(), fstart_core::services::ServiceError>;
    fn early_init(&mut self) -> Result<(), fstart_core::services::ServiceError>;
    /// Reopen stage-local decode before ramstage PCI enumeration. Chipsets
    /// with pre-DRAM device selection must preserve it rather than re-probe.
    fn ramstage_early_init(&mut self) -> Result<(), fstart_core::services::ServiceError> {
        self.early_init()
    }
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

    /// Upper bound for firmware destinations, including the persistent store.
    /// Chipsets with a trained low-memory map exclude their top reservations.
    fn bootstrap_ram_end(&self) -> Result<u64, fstart_core::services::ServiceError> {
        self.total_ram_bytes()
    }

    /// Bootblock-only sampling (e.g. CPU replacement and DRAM power state)
    /// before the southbridge starts its native PHY setup.
    fn prepare_training(&mut self) -> Result<(), fstart_core::services::ServiceError> {
        Ok(())
    }

    /// Lock chipset resources before handoff after ACPI/SMM installation.
    fn finalize_init(&mut self) -> Result<(), fstart_core::services::ServiceError> {
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
    /// The board's values of the options, from its configuration alone:
    /// what the driver starts from, and what a build's option schema lists.
    fn board_options(config: &Self::Config) -> Self::Options;
    /// The values the driver runs with.
    fn options(&self) -> Self::Options;
    fn set_options(&mut self, options: Self::Options);

    fn new_from_config(
        config: &'static Self::Config,
    ) -> Result<Self, fstart_core::services::ServiceError>;
    fn config(&self) -> &'static Self::Config;
    fn pre_console_init(&mut self) -> Result<(), fstart_core::services::ServiceError>;
    /// Pre-console setup at reset, not when the ramstage rebinds LPC.
    fn bootblock_pre_console_init(&mut self) -> Result<(), fstart_core::services::ServiceError> {
        self.pre_console_init()
    }
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
    /// Whether unlisted onboard (bus-0) functions get coreboot's default
    /// subsystem ID through [`Self::program_subsystem_id`] with `id = 0`:
    /// only chipsets whose callback maps zero to the function's own VID/DID.
    fn onboard_subsystem_fallback(&self) -> bool {
        false
    }
    /// Chipset-owned PCI BARs that generic resource allocation must preserve.
    fn fixed_pci_bars(&self) -> fstart_pci::PciFixedBars {
        fstart_pci::PciFixedBars::new()
    }
    /// Can the requested population be replayed over this cold ACPI snapshot?
    /// Checked on resume before `chip_init` can disable or renumber ports.
    fn matches_acpi_population(&self, _cold: &[u8]) -> bool {
        true
    }
    /// Capture the live allocation on which persistent ACPI descriptions depend,
    /// before any PCI functions are hidden. Empty for fully PCI-described devices.
    fn acpi_bar_snapshot(
        &mut self,
    ) -> Result<alloc::vec::Vec<u8>, fstart_core::services::ServiceError> {
        Ok(alloc::vec::Vec::new())
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
    /// Notify the southbridge that the platform enabled the EHCI debug console
    /// on bus-0 `devfn` at fixed MMIO `bar`, so USB initialization preserves
    /// that controller's BAR, active state, and debug port routing.
    fn set_ehci_debug(&mut self, _devfn: u8, _bar: u32) {}
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
    /// Complete southbridge power-management init that must run after MP CPU
    /// C-state setup and northbridge `BIOS_RESET_CPL` (e.g. Lynx Point-LP
    /// PM_SYNC enable, dynamic clock gating, and `GEN_PMCON_2.DRAM_INIT`).
    fn post_mp_init(&mut self) -> Result<(), fstart_core::services::ServiceError> {
        Ok(())
    }
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
