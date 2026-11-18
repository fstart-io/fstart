//! Intel CAR platforms: typed chipset pairings and the phases a board's
//! stage `main`s call.
//!
//! Pairings supply hardware types and constants. The order inside a phase
//! (DRAM training, DMI, S3 decisions) lives once here; the order of the
//! phases and the board's work between them lives in the board's
//! `src/bin/{bootblock,postcar,ramstage}.rs`.

#![no_std]

/// SMBIOS Type 0 release date selected by fbuild, defaulting to the build date.
pub const SMBIOS_RELEASE_DATE: &str = env!("FSTART_SMBIOS_DATE");

extern crate ufmt;

pub mod tables;

pub mod debug_console;
pub mod facts;
pub mod memory_cache;

/// How the FFS stores what this platform's stages load. The host plan
/// compresses each image with its constant and the stage loading it passes
/// `codec` of the same constant, so a stage links only the decoders it uses.
pub mod compression {
    use fstart_core::Compression;
    /// Postcar, loaded by the bootblock: stored, so the bootblock links no
    /// decoder.
    pub const POSTCAR: Compression = Compression::None;
    /// The ramstage, loaded by postcar.
    pub const RAMSTAGE: Compression = Compression::Lz4;
    /// The payload's files, loaded by the ramstage.
    pub const PAYLOAD: Compression = Compression::Lz4;
}

pub use facts::{IntelBoardFacts, IntelPlatform, IntelPlatformConfig};
#[cfg(feature = "host")]
pub mod host;
#[cfg(feature = "stage")]
#[doc(hidden)]
pub use fstart_arch as arch_runtime;
#[cfg(feature = "stage")]
use fstart_core::services::ServiceError;
#[cfg(feature = "bundle-smm")]
pub use fstart_smm as smm;
#[cfg(feature = "stage")]
#[doc(hidden)]
pub use fstart_stage as stage_runtime;
#[cfg(feature = "host")]
pub use host::Plan;

/// Declare a board-owned stage `main`: `fstart_platform_intel::entry!(main);`.
/// The bootblock, which the CPU enters at the reset vector, adds it:
/// `entry!(main, reset_vector);`. Dependency names stay inside the platform.
#[macro_export]
macro_rules! entry {
    ($main:path) => {
        $crate::stage_runtime::stage_bin!(main: $main);
    };
    ($main:path, reset_vector) => {
        $crate::stage_runtime::stage_bin!(main: $main);
        $crate::arch_runtime::x86_reset_entry!();
    };
}

pub mod baytrail;
pub mod gm45;
pub mod gm965;
pub mod i945;
pub mod ironlake;
pub mod layout;
pub mod legacy_cpu;
pub mod pineview;

/// Shared IGD display bring-up types used by board display policy.
pub use fstart_driver_intel::igd;

#[cfg(feature = "stage")]
mod boot;
#[cfg(feature = "stage")]
pub mod bootblock;
#[cfg(feature = "stage")]
mod memory_info;
#[cfg(feature = "stage")]
pub mod options;
#[cfg(feature = "stage")]
pub mod postcar;
#[cfg(feature = "ramstage")]
pub mod ramstage;
#[cfg(feature = "stage")]
mod store;

#[cfg(feature = "stage")]
pub use fstart_driver_intel::{
    BootPath, IntelEcamConfig, IntelNorthbridgeDriver, IntelSouthbridgeDriver,
};

/// Native SMM handler image built by fbuild, embedded into stages whose build
/// had `FSTART_SMM_IMAGE` set (the DRAM mainstage of SMM-capable boards).
///
/// `None` in stages built without an SMM image (bootblocks, non-SMM boards);
/// MP setup then skips SMM relocation and SMRAM stays unlocked.
#[cfg(all(feature = "stage", fstart_intel_has_smm_image))]
pub const SMM_IMAGE: Option<&'static [u8]> = Some(include_bytes!(env!("FSTART_SMM_IMAGE")));
#[cfg(all(feature = "stage", not(fstart_intel_has_smm_image)))]
pub const SMM_IMAGE: Option<&'static [u8]> = None;

// ---------------------------------------------------------------------------
// Chipset and board contracts
// ---------------------------------------------------------------------------

/// An Intel hardware binding driven by the shared flow below. Legacy chipset
/// pairs take a board-selected CPU-package parameter; newer platforms can
/// supply their CPU family directly. The shared flow owns the stage transitions,
/// training cache and PCI allocator; the binding owns the hardware handles and
/// the order in which it calls the chipset blocks within each phase.
#[cfg(feature = "stage")]
pub trait IntelEarlyPlatform:
    Sized
    + IntelPlatform<
        Config: IntelChipsetConfig<
            Northbridge = Self::Northbridge,
            Southbridge = Self::Southbridge,
        >,
    >
{
    /// Re-run native display init (modeset + OpRegion) on the S3 resume path.
    /// Chipsets whose OS display driver restores the screen leave this false
    /// so resume skips the modeset flicker.
    const RESUME_DISPLAY_INIT: bool;
    /// Take platform-owned CAR scratch once for cached input and new training.
    /// A cache-capable platform supplies its own workspace; the common flow
    /// only borrows slices and never selects or allocates a payload size.
    fn training_buffers() -> Option<crate::memory_cache::TrainingBuffers<'static>> {
        None
    }
    type Northbridge: IntelNorthbridgeDriver
        + fstart_arch::x86::cpu::intel::smm::SmramControl
        + NorthbridgeAcpi<Self::Northbridge>;
    type Southbridge: IntelSouthbridgeDriver
        + fstart_core::services::SmBus
        + fstart_arch::x86::cpu::intel::smm::SmiControl
        + SouthbridgeAcpi<Self::Southbridge>;

    /// Construct the chipset owners while CAR is live, before opening the
    /// console decodes. The default adapts the existing NB/SB pairing.
    fn bind_bootblock(
        config: &'static Self::Config,
    ) -> Result<(Self::Northbridge, Self::Southbridge), ServiceError> {
        let north = Self::Northbridge::new_from_config(config.northbridge());
        let south = Self::Southbridge::new_from_config(config.southbridge());
        Ok((north?, south?))
    }

    /// Bind fresh hardware owners in DRAM; no owner is carried out of CAR.
    fn bind_ramstage(
        config: &'static Self::Config,
    ) -> Result<(Self::Northbridge, Self::Southbridge), ServiceError> {
        let north = Self::Northbridge::new_from_config(config.northbridge())?;
        let south = Self::Southbridge::new_from_config(config.southbridge())?;
        Ok((north, south))
    }

    /// Bind the existing ECAM decode for a postcar debug console. Do not
    /// acquire CAR storage or repeat memory training in postcar.
    fn postcar_console_decode(config: &'static Self::Config) -> Result<(), ServiceError> {
        Self::Northbridge::new_from_config(config.northbridge())?.pre_console_init()
    }

    /// Open the chipset decodes needed before the bootblock or ramstage console.
    fn pre_console_init(
        north: &mut Self::Northbridge,
        south: &mut Self::Southbridge,
    ) -> Result<(), ServiceError> {
        north.pre_console_init()?;
        south.pre_console_init()
    }

    /// Finish early chipset setup after the bootblock console and its options.
    fn bootblock_early_init(
        north: &mut Self::Northbridge,
        south: &mut Self::Southbridge,
    ) -> Result<(), ServiceError> {
        north.early_init()?;
        south.early_init()
    }

    /// Prepare for RAM init once, before the shared flow reads any training cache.
    fn pre_raminit(
        _north: &mut Self::Northbridge,
        south: &mut Self::Southbridge,
    ) -> Result<(), ServiceError> {
        south.pre_raminit_init()
    }

    /// Train or restore RAM. The shared flow owns the cache and lends its buffers;
    /// the binding provides the actual hardware inputs to the memory controller.
    fn train_memory(
        north: &mut Self::Northbridge,
        south: &mut Self::Southbridge,
        cached: Option<&[u8]>,
        capture: &mut [u8],
    ) -> Result<Option<usize>, ServiceError> {
        use fstart_core::services::MemoryInit;
        let smbus = south.smbus_mut().ok_or(ServiceError::NotInitialized)?;
        north.bring_up(fstart_driver_intel::SpdTraining {
            smbus,
            cached,
            capture,
        })
    }

    /// Complete both sides of the chipset link before the bootblock loads
    /// postcar. The default keeps the legacy NB/SB ordering.
    fn finish_memory_init(
        north: &mut Self::Northbridge,
        south: &mut Self::Southbridge,
    ) -> Result<(), ServiceError> {
        south.prepare_early_post_dram_init()?;
        north.early_post_dram_init()?;
        south.early_post_dram_init()?;
        north.finish_early_post_dram_init()
    }

    /// Finish early chipset setup after the ramstage console and its options.
    fn ramstage_early_init(
        north: &mut Self::Northbridge,
        south: &mut Self::Southbridge,
    ) -> Result<(), ServiceError> {
        north.ramstage_early_init()?;
        south.early_init()
    }

    /// Set up chipset functions during memory-map initialization, before the
    /// board's pre-scan work and the common PCI enumeration.
    fn init_chipset_functions(
        north: &mut Self::Northbridge,
        south: &mut Self::Southbridge,
    ) -> Result<(), ServiceError> {
        north.chip_init()?;
        south.chip_init()
    }

    /// Finish chipset device init after PCI allocation, final page tables and
    /// subsystem IDs.
    fn init_chipset_devices(
        north: &mut Self::Northbridge,
        south: &mut Self::Southbridge,
    ) -> Result<(), ServiceError> {
        north.post_dram_init()?;
        south.post_dram_init()
    }

    /// CPU family driver for MP bring-up; also supplies the CPU-model facts
    /// the shared SMM installer needs.
    #[cfg(feature = "stage")]
    type Cpu: fstart_arch::x86::mp::CpuDriver + fstart_arch::x86::cpu::intel::smm::SmmCpu;
    #[cfg(feature = "stage")]
    fn cpu_driver(
        microcode: Option<&'static [u8]>,
        northbridge: &Self::Northbridge,
        southbridge: &Self::Southbridge,
    ) -> Self::Cpu;
}

/// ACPI contribution required of a northbridge driver.
#[cfg(feature = "stage")]
pub trait NorthbridgeAcpi<NB: IntelNorthbridgeDriver>:
    fstart_acpi::device::AcpiDevice<Config = NB::Config>
{
}
#[cfg(feature = "stage")]
impl<NB, T> NorthbridgeAcpi<NB> for T
where
    NB: IntelNorthbridgeDriver,
    T: fstart_acpi::device::AcpiDevice<Config = NB::Config>,
{
}

/// ACPI contribution required of a southbridge driver.
#[cfg(feature = "stage")]
pub trait SouthbridgeAcpi<SB: IntelSouthbridgeDriver>:
    fstart_driver_intel::namespace::SouthbridgeAcpi + fstart_acpi::platform::X86PlatformProvider
{
}
#[cfg(feature = "stage")]
impl<SB, T> SouthbridgeAcpi<SB> for T
where
    SB: IntelSouthbridgeDriver,
    T: fstart_driver_intel::namespace::SouthbridgeAcpi + fstart_acpi::platform::X86PlatformProvider,
{
}

/// Built chipset policy: the derived driver configs and the CPU population.
#[cfg(feature = "stage")]
pub trait IntelChipsetConfig: IntelPlatformConfig {
    type Northbridge: IntelNorthbridgeDriver;
    type Southbridge: IntelSouthbridgeDriver;
    fn northbridge(&'static self)
    -> &'static <Self::Northbridge as IntelNorthbridgeDriver>::Config;
    fn southbridge(&'static self)
    -> &'static <Self::Southbridge as IntelSouthbridgeDriver>::Config;
}

/// A board on an Intel CAR platform: its hardware binding and facts. The
/// board's stage `main`s drive [`bootblock`], [`postcar`] and [`ramstage`].
#[cfg(feature = "stage")]
pub trait IntelBoard: IntelBoardFacts<Platform: IntelEarlyPlatform> + 'static {}
#[cfg(feature = "stage")]
impl<B> IntelBoard for B where B: IntelBoardFacts<Platform: IntelEarlyPlatform> + 'static {}

/// FFS name of the DRAM mainstage.
pub const RAMSTAGE_NAME: &str = "ramstage";

/// Canonical role name (see `fstart_core::stage::POSTCAR_STAGE_NAME`):
/// all Intel CAR boards insert a stage with this name between bootblock and
/// ramstage. `fbuild` selects the postcar entry by it.
/// It names a stage *role*, not a board registry.
pub use fstart_core::stage::POSTCAR_STAGE_NAME;
