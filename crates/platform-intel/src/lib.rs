//! Shared handwritten Intel flows and typed chipset pairings.
//!
//! Pairings supply hardware types and constants; initialization order lives
//! once in the family bootblock, postcar and mainstage flows.

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
pub mod smbus;
pub use facts::{IntelBoardFacts, IntelPlatform, IntelPlatformConfig};
pub use smbus::{IntelSmbusRouting, SmbusRoute};
#[cfg(feature = "host")]
pub mod host;
#[cfg(feature = "bundle-smm")]
pub use fstart_smm as smm;
#[cfg(feature = "stage")]
#[doc(hidden)]
pub use fstart_stage as stage_runtime;
#[cfg(feature = "host")]
pub use host::Plan;

/// Hygienic firmware entry; dependency names stay inside the platform.
#[macro_export]
macro_rules! stage_bin {
    ($board:ty) => { $crate::stage_runtime::stage_bin!(program: $crate::Program<$board>); };
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

#[cfg(all(
    feature = "stage",
    any(
        fstart_stage_env = "car",
        fstart_stage_env = "postcar",
        fstart_stage_env = "ram"
    )
))]
mod boot;
#[cfg(all(feature = "stage", fstart_stage_env = "car"))]
mod bootblock;
#[cfg(all(feature = "stage", fstart_stage_env = "ram"))]
mod mainstage;
#[cfg(all(
    feature = "stage",
    any(fstart_stage_env = "car", fstart_stage_env = "ram")
))]
mod memory_info;
#[cfg(all(feature = "stage", fstart_stage_env = "postcar"))]
mod postcar;
#[cfg(all(
    feature = "stage",
    any(
        fstart_stage_env = "car",
        fstart_stage_env = "postcar",
        fstart_stage_env = "ram"
    )
))]
mod store;
#[cfg(all(feature = "stage", fstart_stage_env = "ram"))]
pub use mainstage::{IntelMainstage, Mainstage, MainstageCtx};

#[cfg(feature = "stage")]
use core::marker::PhantomData;
#[cfg(feature = "stage")]
use fstart_core::services::{ConsoleDevice, ServiceError};
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

/// The concatenated Intel microcode blob, as a verified copy in RAM.
///
/// The bootblock applies BSP microcode before CAR setup; MP init calls this
/// so every AP gets the same update. Each CPU parses the blob and hands the
/// selected record to the update MSR, so they read a copy authenticated
/// against its FFS digest instead of flash: a flash read glitch would
/// otherwise break the parse, and reading uncached flash is slow. The copy
/// is 16-byte aligned as `IA32_BIOS_UPDT_TRIG` requires and lives for the
/// stage. Returns `None` when no window is mounted or the image has no blob.
#[cfg(all(
    feature = "stage",
    feature = "mp",
    any(target_arch = "x86", target_arch = "x86_64")
))]
#[must_use]
pub fn intel_microcode_blob() -> Option<&'static [u8]> {
    extern crate alloc;
    use alloc::alloc::{Layout, alloc};
    use fstart_core::services::boot_media::MemoryMapped;

    let ctx = fstart_core::services::ffs_context::memory_mapped()?;
    let size = usize::try_from(ctx.image_size).ok()?;
    // SAFETY: the boot-media provider published this window for the stage.
    let media = unsafe { MemoryMapped::from_raw_addr(ctx.image_base, size) };
    // SAFETY: documented valid for the whole stage once published.
    let anchor = unsafe { ctx.anchor_bytes() };
    let verified =
        fstart_stage::find_ffs_file_data(anchor, &media, fstart_core::ffs::FileType::CpuMicrocode)
            .filter(|blob| !blob.is_empty())?;
    let layout = Layout::from_size_align(verified.len(), 16).ok()?;
    // SAFETY: nonzero size; never freed.
    let copy = unsafe { alloc(layout) };
    if copy.is_null() {
        return None;
    }
    // SAFETY: fresh allocation of exactly this size.
    unsafe {
        core::ptr::copy_nonoverlapping(verified.as_ptr(), copy, verified.len());
        Some(core::slice::from_raw_parts(copy, verified.len()))
    }
}

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

/// Board contract for the Intel flow.
#[cfg(feature = "stage")]
pub trait IntelBoard: Sized + 'static + IntelBoardFacts<Platform: IntelEarlyPlatform> {
    #[cfg(fstart_stage_env = "car")]
    type EarlyHooks: IntelEarlyBoardHooks<Self::Platform> + Default;
    #[cfg(fstart_stage_env = "ram")]
    type MainstageHooks: IntelMainstageBoardHooks<Self::Platform> + Default;
    type Console: ConsoleDevice;

    fn console_config() -> <Self::Console as ConsoleDevice>::Config;
    fn console_node() -> &'static str;
    #[cfg(feature = "smbios")]
    fn smbios_identity() -> &'static crate::tables::SmbiosIdentity<'static>;
}

/// Platform-owned adapter for the fixed Intel stage dispatch.
#[cfg(feature = "stage")]
pub struct Program<B>(PhantomData<B>);

#[cfg(feature = "stage")]
impl<B: IntelBoard> fstart_stage::StageProgram for Program<B> {
    fn run_stage(_handoff: usize) -> ! {
        #[cfg(fstart_stage_env = "car")]
        {
            let mut hooks = B::EarlyHooks::default();
            let flow = bootstrap_spec::<B>(0)
                .and_then(|spec| bootblock::run_intel_bootblock::<B>(spec, &mut hooks));
            if flow.is_err() {
                fstart_log::error!("{} bootblock failed", B::Platform::NAME);
            }
            fstart_arch::x86::boot::halt()
        }
        #[cfg(fstart_stage_env = "postcar")]
        {
            // Teardown already done by the entry; load the ramstage cached.
            let Ok(spec) = bootstrap_spec::<B>(1) else {
                fstart_arch::x86::boot::halt()
            };
            postcar::run_intel_postcar::<B::Console>(spec)
        }
        #[cfg(fstart_stage_env = "ram")]
        {
            mainstage::run_intel_mainstage::<B>()
        }
        #[cfg(not(any(
            fstart_stage_env = "car",
            fstart_stage_env = "postcar",
            fstart_stage_env = "ram"
        )))]
        panic!("Intel boards require a fixed stage selection");
    }
}

#[cfg(all(
    feature = "stage",
    any(fstart_stage_env = "car", fstart_stage_env = "postcar")
))]
fn bootstrap_spec<B: IntelBoard>(index: u16) -> Result<FfsLoadSpec<B::Console>, ServiceError> {
    Ok(FfsLoadSpec {
        platform: B::Platform::NAME,
        geometry: layout::IntelBootLayout::current(index)?,
        console_config: B::console_config(),
        console_node: B::console_node(),
    })
}

/// FFS name of the DRAM mainstage.
pub const RAMSTAGE_NAME: &str = "ramstage";

#[cfg(all(
    feature = "stage",
    any(fstart_stage_env = "car", fstart_stage_env = "postcar")
))]
pub(crate) struct FfsLoadSpec<C: ConsoleDevice> {
    pub platform: &'static str,
    pub geometry: layout::IntelBootLayout<'static>,
    pub console_config: C::Config,
    pub console_node: &'static str,
}

/// Shared Intel mainstage state: the chipset drivers bound from typed config
/// plus the flow-owned context.
/// Canonical role name (see `fstart_core::stage::POSTCAR_STAGE_NAME`):
/// all Intel CAR boards insert a stage with this name between bootblock and
/// ramstage. `fbuild` selects the postcar entry and stage environment by it,
/// and `run_stage` dispatches to the postcar loader by
/// `cfg(fstart_stage_env = "postcar")`. It names a stage *role*,
/// not a board registry.
pub use fstart_core::stage::POSTCAR_STAGE_NAME;

// ---------------------------------------------------------------------------
// Board hooks
// ---------------------------------------------------------------------------

/// Mainstage board hooks contribute ACPI fragments through the same [`AcpiDevice`]
/// abstraction chipset drivers use. Vacuous when ACPI is disabled.
///
/// [`AcpiDevice`]: fstart_acpi::device::AcpiDevice
#[cfg(all(feature = "stage", fstart_stage_env = "ram", feature = "acpi"))]
pub trait MainboardAcpi<P: IntelEarlyPlatform>:
    fstart_acpi::device::AcpiDevice<Config = P::AcpiContext>
{
}
#[cfg(all(feature = "stage", fstart_stage_env = "ram", feature = "acpi"))]
impl<P, T> MainboardAcpi<P> for T
where
    P: IntelEarlyPlatform,
    T: fstart_acpi::device::AcpiDevice<Config = P::AcpiContext>,
{
}
#[cfg(all(feature = "stage", fstart_stage_env = "ram", not(feature = "acpi")))]
pub trait MainboardAcpi<P> {}
#[cfg(all(feature = "stage", fstart_stage_env = "ram", not(feature = "acpi")))]
impl<P, T> MainboardAcpi<P> for T {}

/// CAR-only context. LPC decode is active; no heap or PCI allocation exists.
#[cfg(all(feature = "stage", fstart_stage_env = "car"))]
pub struct IntelEarlyCtx<'a, P: IntelEarlyPlatform> {
    southbridge: &'a mut P::Southbridge,
    pub boot_path: fstart_driver_intel::BootPath,
}

#[cfg(all(feature = "stage", fstart_stage_env = "car"))]
impl<'a, P: IntelEarlyPlatform> IntelEarlyCtx<'a, P> {
    pub(crate) fn new(southbridge: &'a mut P::Southbridge) -> Self {
        Self {
            southbridge,
            boot_path: fstart_driver_intel::BootPath::Normal,
        }
    }
    pub(crate) fn with_boot_path(
        southbridge: &'a mut P::Southbridge,
        boot_path: fstart_driver_intel::BootPath,
    ) -> Self {
        Self {
            southbridge,
            boot_path,
        }
    }

    #[must_use]
    pub fn southbridge(&mut self) -> &mut P::Southbridge {
        self.southbridge
    }
}

/// Bootblock hooks, called on cold, warm and S3 boots. State is CAR-local
/// and is not carried into mainstage. Before console, only chipset decode
/// is ready; before memory, chipset early init is complete; after memory,
/// DRAM training/recovery is complete. Handoff follows authentication/loading.
#[cfg(all(feature = "stage", fstart_stage_env = "car"))]
pub trait IntelEarlyBoardHooks<P: IntelEarlyPlatform>: IntelSmbusRouting<P::Southbridge> {
    fn before_console(&mut self, _ctx: &mut IntelEarlyCtx<P>) -> Result<(), ServiceError> {
        Ok(())
    }

    fn before_memory(&mut self, _ctx: &mut IntelEarlyCtx<P>) -> Result<(), ServiceError> {
        Ok(())
    }

    fn after_memory(&mut self, _ctx: &mut IntelEarlyCtx<P>) -> Result<(), ServiceError> {
        Ok(())
    }

    fn before_handoff(&mut self, _ctx: &mut IntelEarlyCtx<P>) -> Result<(), ServiceError> {
        Ok(())
    }
}

/// Mainstage-only context. DRAM/heap exist throughout; the memory map is
/// populated after console bring-up, and PCI resources are assigned before
/// `after_devices`. `resume` identifies the S3 path, not an ordinary cold boot.
#[cfg(all(feature = "stage", fstart_stage_env = "ram"))]
pub struct IntelMainstageBoardCtx<'a, P: IntelEarlyPlatform> {
    pub(crate) southbridge: &'a mut P::Southbridge,
    pub(crate) memory: &'a MainstageCtx,
    pub resume: bool,
}

#[cfg(all(feature = "stage", fstart_stage_env = "ram"))]
impl<P: IntelEarlyPlatform> IntelMainstageBoardCtx<'_, P> {
    pub fn southbridge(&mut self) -> &mut P::Southbridge {
        self.southbridge
    }

    pub fn memory(&self) -> &MainstageCtx {
        self.memory
    }
}

/// Fresh mainstage board state, never reconstructed by replaying early hooks.
/// Runs on cold/warm/S3 boots: establish board console routing after LPC decode,
/// initialize board devices after chipset/PCI setup, then prepare OS handoff
/// after tables. Hardware setup does not depend on ACPI emission.
#[cfg(all(feature = "stage", fstart_stage_env = "ram"))]
pub trait IntelMainstageBoardHooks<P: IntelEarlyPlatform>:
    MainboardAcpi<P> + IntelSmbusRouting<P::Southbridge>
{
    /// Override static board identity with data read from board-owned hardware.
    #[cfg(feature = "smbios")]
    fn smbios_identity<'a>(
        &'a self,
        configured: &crate::tables::SmbiosIdentity<'a>,
    ) -> crate::tables::SmbiosIdentity<'a> {
        *configured
    }

    fn before_console(&mut self, _ctx: &mut IntelMainstageBoardCtx<P>) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Sample board population policy after PCI allocation but before chipset
    /// device programming, e.g. a removable dock's primary IDE channel.
    fn before_devices(&mut self, _ctx: &mut IntelMainstageBoardCtx<P>) -> Result<(), ServiceError> {
        Ok(())
    }

    fn after_devices(&mut self, _ctx: &mut IntelMainstageBoardCtx<P>) -> Result<(), ServiceError> {
        Ok(())
    }

    fn before_handoff(&mut self, _ctx: &mut IntelMainstageBoardCtx<P>) -> Result<(), ServiceError> {
        Ok(())
    }
}
