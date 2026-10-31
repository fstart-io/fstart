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

#[cfg(any(feature = "stage", feature = "acpi", feature = "smbios"))]
extern crate ufmt;

#[cfg(any(feature = "host", feature = "acpi", feature = "smbios"))]
#[cfg_attr(
    all(feature = "host", not(any(feature = "acpi", feature = "smbios"))),
    allow(dead_code)
)]
pub mod tables;

pub mod facts;
pub mod memory_cache;
pub use facts::{IntelBoardFacts, IntelPlatform, IntelPlatformConfig};
#[cfg(feature = "host")]
pub mod host;
#[cfg(feature = "bundle-smm")]
pub use fstart_smm as smm;
#[cfg(feature = "stage")]
#[doc(hidden)]
pub use fstart_stage as stage_runtime;
#[cfg(feature = "host")]
pub use host::Plan;

/// Declare a board-owned stage `main`: `fstart_platform_intel::entry!(main);`
/// Dependency names stay inside the platform.
#[macro_export]
macro_rules! entry {
    ($main:path) => { $crate::stage_runtime::stage_bin!(main: $main); };
}

#[cfg(feature = "gm965-ich8")]
pub mod gm965;
#[cfg(feature = "i945-ich7")]
pub mod i945;
pub mod layout;
pub mod legacy_cpu;
#[cfg(feature = "pineview-ich7")]
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
pub mod postcar;
#[cfg(feature = "stage")]
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
    #[cfg(feature = "acpi")]
    const RESUME_DISPLAY_INIT: bool;
    type Northbridge: IntelNorthbridgeDriver
        + fstart_arch::x86::cpu::intel::smm::SmramControl
        + NorthbridgeAcpi<Self::Northbridge>;
    type Southbridge: IntelSouthbridgeDriver
        + fstart_core::services::SmBus
        + fstart_arch::x86::cpu::intel::smm::SmiControl
        + SouthbridgeAcpi<Self::Southbridge>;
    /// CPU family driver for MP bring-up; also supplies the CPU-model facts
    /// the shared SMM installer needs.
    #[cfg(feature = "mp")]
    type Cpu: fstart_arch::x86::mp::CpuDriver + fstart_arch::x86::cpu::intel::smm::SmmCpu;
    #[cfg(feature = "mp")]
    fn cpu_driver(microcode: Option<&'static [u8]>) -> Self::Cpu;
    /// Platform-owned ACPI namespace context handed to `AcpiDevice` emitters.
    #[cfg(feature = "acpi")]
    type AcpiContext: Default;
}

/// ACPI contribution required of a northbridge driver; vacuous without ACPI.
#[cfg(all(feature = "stage", feature = "acpi"))]
pub trait NorthbridgeAcpi<NB: IntelNorthbridgeDriver>:
    fstart_acpi::device::AcpiDevice<Config = NB::Config>
{
}
#[cfg(all(feature = "stage", feature = "acpi"))]
impl<NB, T> NorthbridgeAcpi<NB> for T
where
    NB: IntelNorthbridgeDriver,
    T: fstart_acpi::device::AcpiDevice<Config = NB::Config>,
{
}
#[cfg(all(feature = "stage", not(feature = "acpi")))]
pub trait NorthbridgeAcpi<NB> {}
#[cfg(all(feature = "stage", not(feature = "acpi")))]
impl<NB, T> NorthbridgeAcpi<NB> for T {}

/// ACPI contribution required of a southbridge driver; vacuous without ACPI.
#[cfg(all(feature = "stage", feature = "acpi"))]
pub trait SouthbridgeAcpi<SB: IntelSouthbridgeDriver>:
    fstart_acpi::device::AcpiDevice<Config = SB::Config> + fstart_acpi::platform::X86PlatformProvider
{
}
#[cfg(all(feature = "stage", feature = "acpi"))]
impl<SB, T> SouthbridgeAcpi<SB> for T
where
    SB: IntelSouthbridgeDriver,
    T: fstart_acpi::device::AcpiDevice<Config = SB::Config>
        + fstart_acpi::platform::X86PlatformProvider,
{
}
#[cfg(all(feature = "stage", not(feature = "acpi")))]
pub trait SouthbridgeAcpi<SB> {}
#[cfg(all(feature = "stage", not(feature = "acpi")))]
impl<SB, T> SouthbridgeAcpi<SB> for T {}

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
