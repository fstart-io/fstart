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

pub mod acpi;
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
#[doc(hidden)]
pub use fstart_core as core_runtime;
#[cfg(feature = "bundle-smm")]
#[doc(hidden)]
pub use fstart_driver_intel as driver_intel_runtime;
#[cfg(feature = "stage")]
#[doc(hidden)]
pub use fstart_log as log_runtime;
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

/// Declare a standard Intel postcar stage binary.
///
/// Boards with a serial console pass `(Board, ConsoleType, console_config)`;
/// boards without a fixed serial port pass `(Board)` to use `NoConsole`.
#[macro_export]
macro_rules! postcar_bin {
    ($board:ty $(,)?) => {
        static CONSOLE: $crate::log_runtime::StaticConsole<
            $crate::core_runtime::services::NoConsole,
        > = $crate::log_runtime::StaticConsole::new();

        $crate::entry!(main);

        fn main() -> ! {
            $crate::postcar::run::<$board, _>(&CONSOLE, ())
        }
    };
    ($board:ty, $console:ty, $config:expr $(,)?) => {
        static CONSOLE: $crate::log_runtime::StaticConsole<$console> =
            $crate::log_runtime::StaticConsole::new();

        $crate::entry!(main);

        fn main() -> ! {
            $crate::postcar::run::<$board, _>(&CONSOLE, $config)
        }
    };
}

/// Declare the default Intel ICH SMM handler binary (APMC/ACPI, TCO, GPE).
/// Boards with custom EC/dock SMI handling use `fstart_platform_intel::smm::smm_bin!`
/// directly with their own handler.
#[macro_export]
macro_rules! smm_default_bin {
    () => {
        $crate::smm::smm_bin!(handler: $crate::driver_intel_runtime::southbridge::smi::handle);
    };
}

/// Reject a platform that cannot train memory because no DIMM slots were wired.
const fn require_spd_addresses(addresses: [u8; 4]) {
    assert!(
        addresses[0] != 0 || addresses[1] != 0 || addresses[2] != 0 || addresses[3] != 0,
        "the board must wire its DIMMs: spd_addresses()"
    );
}

pub mod baytrail;
pub mod gm45;
pub mod gm965;
pub mod haswell;
pub mod i945;
pub mod ironlake;
pub mod layout;
pub mod legacy_cpu;
pub mod pineview;
pub mod skylake;

/// Shared IGD display bring-up types used by board display policy.
pub use fstart_driver_intel::igd;

#[cfg(any(test, feature = "ramstage"))]
mod acpi_bars;
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
#[cfg(any(test, feature = "ramstage"))]
pub mod tpm;

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
/// supply their CPU family directly. The family flow is not duplicated.
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
    /// Haswell loads microcode a second time after SMBASE relocation, before
    /// general model setup. Older CPUs enable alternative SMRR during setup.
    const RELOCATE_BEFORE_CPU_INIT: bool = false;
    /// GSI0, rather than LAPIC LINT0, delivers the 8259 output.
    const IOAPIC_VIRTUAL_WIRE: bool = false;
    /// Reset from postcar, where chipset objects have not been constructed.
    fn reset_failed_postcar_resume() -> ! {
        fstart_arch::x86::boot::system_reset(true)
    }
    /// The bootblock caches the BIOS window write-protect itself, once the
    /// chipset decodes it: the platform's cache-as-RAM entry (Intel enhanced
    /// NEM) leaves the ROM uncached while it fills CAR. The other entries
    /// cache the ROM themselves.
    const CACHE_BIOS_WINDOW: bool = false;
    /// Take platform-owned CAR scratch once for cached input and new training.
    /// A cache-capable platform supplies its own workspace; the common flow
    /// only borrows slices and never selects or allocates a payload size.
    fn training_buffers() -> Option<crate::memory_cache::TrainingBuffers<'static>> {
        None
    }
    /// Take platform-owned CAR storage for a copy of the bootblock's entry
    /// page tables so `--debug flash-console` can program SPI flash without
    /// page-table walks hitting the busy flash chip. Only platforms with
    /// ample CAR space (e.g. Skylake 256 KiB CAR) opt in.
    #[cfg(all(
        feature = "flash-console",
        feature = "bundle-bootblock",
        target_arch = "x86_64"
    ))]
    fn bootblock_car_page_tables()
    -> Option<&'static mut fstart_arch::x86::boot::paging::EntryTables> {
        None
    }
    type Northbridge: IntelNorthbridgeDriver
        + fstart_arch::x86::cpu::intel::smm::SmramControl
        + NorthbridgeAcpi<Self::Northbridge>;
    type Southbridge: IntelSouthbridgeDriver
        + fstart_core::services::SmBus
        + fstart_arch::x86::cpu::intel::smm::SmiControl
        + SouthbridgeAcpi<Self::Southbridge>;
    /// Train DRAM, or replay `cached`: the northbridge's
    /// [`MemoryInit`](fstart_core::services::MemoryInit) from the SPDs on the
    /// southbridge's SMBus. A platform whose memory controller needs more of
    /// its southbridge than the SMBus overrides this.
    fn train_memory(
        northbridge: &mut Self::Northbridge,
        southbridge: &mut Self::Southbridge,
        cached: Option<&[u8]>,
        capture: &mut [u8],
    ) -> Result<Option<usize>, fstart_core::services::ServiceError> {
        use fstart_core::services::{MemoryInit, ServiceError};
        let smbus = southbridge
            .smbus_mut()
            .ok_or(ServiceError::NotInitialized)?;
        northbridge.bring_up(fstart_driver_intel::SpdTraining {
            smbus,
            cached,
            capture,
        })
    }
    /// The SPI controller the memory cache and the flash console write
    /// through. A PCH whose SPI controller is its own PCI function
    /// overrides this.
    ///
    /// # Safety
    /// As [`CacheSpi::new`](fstart_driver_intel::southbridge::spi::CacheSpi::new).
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    unsafe fn cache_flash(
        extent: fstart_driver_intel::southbridge::spi::FlashExtent,
    ) -> Result<fstart_driver_intel::southbridge::spi::CacheSpi, fstart_core::services::ServiceError>
    {
        // SAFETY: the caller's contract.
        unsafe { fstart_driver_intel::southbridge::spi::CacheSpi::new(extent) }
    }
    /// Initialize the chipset's devices once PCI allocation placed their
    /// BARs. A platform whose southbridge devices need the northbridge's
    /// (HDA's display codec) overrides this.
    fn init_devices(
        northbridge: &mut Self::Northbridge,
        southbridge: &mut Self::Southbridge,
        _pci: &fstart_pci::PciEcam,
    ) -> Result<(), fstart_core::services::ServiceError> {
        northbridge.post_dram_init()?;
        southbridge.post_dram_init()
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
