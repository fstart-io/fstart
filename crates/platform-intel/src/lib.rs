//! Shared Intel platform machinery and chipset-specific handwritten flows.
//!
//! Intel-common code lives here only when it is shared by multiple Intel
//! chipsets. Ordering stays in each chipset module's handwritten flow.

#![no_std]

#[cfg(any(feature = "stage", feature = "acpi", feature = "smbios"))]
extern crate ufmt;

#[cfg(any(feature = "acpi", feature = "smbios"))]
pub mod tables;

pub mod facts;
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

pub mod gm965;
pub mod i945;
pub mod layout;
pub mod pineview;

/// Shared IGD display bring-up types used by board display policy.
pub use fstart_driver_intel::igd;

#[cfg(feature = "stage")]
mod boot;
#[cfg(all(feature = "stage", fstart_stage_env = "car"))]
mod bootblock;
#[cfg(all(feature = "stage", fstart_stage_env = "ram"))]
mod mainstage;
#[cfg(all(feature = "stage", fstart_stage_env = "postcar"))]
mod postcar;
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
#[cfg(feature = "stage")]
pub use fstart_stage::payload::MainstagePayload;

/// Native SMM handler image built by fbuild, embedded into stages whose build
/// had `FSTART_SMM_IMAGE` set (the DRAM mainstage of SMM-capable boards).
///
/// `None` in stages built without an SMM image (bootblocks, non-SMM boards);
/// MP setup then skips SMM relocation and SMRAM stays unlocked.
#[cfg(all(feature = "stage", fstart_intel_has_smm_image))]
pub const SMM_IMAGE: Option<&'static [u8]> = Some(include_bytes!(env!("FSTART_SMM_IMAGE")));
#[cfg(all(feature = "stage", not(fstart_intel_has_smm_image)))]
pub const SMM_IMAGE: Option<&'static [u8]> = None;

/// Locate the concatenated Intel microcode blob in the mounted memory-mapped
/// FFS window.
///
/// The bootblock applies BSP microcode before CAR setup; MP init calls this
/// so every AP gets the same update. The blob's location is recorded in the
/// image anchor, so this goes through the published FFS context and the FFS
/// reader rather than open-coded flash mappings. Returns `None` when no
/// window is mounted or the anchor records no blob.
#[cfg(all(feature = "stage", feature = "mp", target_arch = "x86_64"))]
#[must_use]
pub fn intel_microcode_blob() -> Option<&'static [u8]> {
    use fstart_ffs::FfsReader;

    let ctx = fstart_core::services::ffs_context::memory_mapped()?;
    // SAFETY: both accessors are documented as valid for the whole stage
    // execution once the boot-media provider published them.
    let (image, anchor_bytes) = unsafe { (ctx.image_bytes(), ctx.anchor_bytes()) };
    // SAFETY: the anchor bytes are the stage's aligned `.fstart.anchor` static.
    let anchor = unsafe { fstart_ffs::FfsReader::read_anchor_volatile(anchor_bytes) }.ok()?;
    FfsReader::new(image).intel_microcode(anchor)
}

// ---------------------------------------------------------------------------
// Chipset and board contracts
// ---------------------------------------------------------------------------

/// A fixed Intel chipset pair (northbridge + southbridge + CPU family) driven
/// by the shared flow below. Chipset modules implement this once; the flow
/// itself is not duplicated per generation.
#[cfg(feature = "stage")]
pub trait IntelEarlyPlatform: Sized + 'static {
    /// Log prefix, e.g. `"pineview/ich7"`.
    const NAME: &'static str;
    /// Re-run native display init (modeset + OpRegion) on the S3 resume path.
    /// Chipsets whose OS display driver restores the screen leave this false
    /// so resume skips the modeset flicker.
    #[cfg(feature = "acpi")]
    const RESUME_DISPLAY_INIT: bool;
    /// Dispatch shared SMI state only on logical CPU 0.
    ///
    /// Pineview requires this because locked exchanges against its TSEG mapping
    /// stall. Other platforms retain normal serialized dispatch.
    #[cfg(feature = "mp")]
    const SMM_BSP_ONLY_DISPATCH: bool = false;
    /// Board-facing chipset policy, built in `.rodata` by the board.
    type Config: IntelChipsetConfig<Northbridge = Self::Northbridge, Southbridge = Self::Southbridge>
        + 'static;
    type Northbridge: IntelNorthbridgeDriver
        + fstart_arch::cpu_intel::smm::SmramControl
        + NorthbridgeAcpi<Self::Northbridge>;
    type Southbridge: IntelSouthbridgeDriver
        + fstart_arch::cpu_intel::smm::SmiControl
        + SouthbridgeAcpi<Self::Southbridge>;
    /// CPU family driver for MP bring-up.
    #[cfg(feature = "mp")]
    type Cpu: fstart_arch::mp::CpuDriver;
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
pub trait IntelChipsetConfig {
    type Northbridge: IntelNorthbridgeDriver;
    type Southbridge: IntelSouthbridgeDriver;
    fn northbridge(&'static self)
    -> &'static <Self::Northbridge as IntelNorthbridgeDriver>::Config;
    fn southbridge(&'static self)
    -> &'static <Self::Southbridge as IntelSouthbridgeDriver>::Config;
    /// Maximum logical CPU count (BSP + APs) the board populates.
    fn max_cpus(&self) -> u16;
}

/// Board contract for the Intel flow.
#[cfg(feature = "stage")]
pub trait IntelBoard: Sized + 'static + crate::facts::IntelBoardFacts {
    type Platform: IntelEarlyPlatform;
    type Hooks: IntelEarlyBoardHooks<Self::Platform>;
    type Console: ConsoleDevice;
    /// Terminal payload launcher; only the DRAM mainstage links one.
    #[cfg(fstart_stage_env = "ram")]
    type Payload: MainstagePayload<Mainstage<Self>>;

    /// Board platform policy. Points at a board `static` so the config lives
    /// in `.rodata`, never on the early-stage stack.
    const CONFIG: &'static <Self::Platform as IntelEarlyPlatform>::Config;

    fn hooks() -> Result<Self::Hooks, ServiceError>;
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
            let Ok(mut hooks) = B::hooks() else {
                fstart_arch::x86_64::halt();
            };
            let flow = bootstrap_spec::<B>(0)
                .and_then(|spec| bootblock::run_intel_bootblock::<B>(spec, &mut hooks));
            if flow.is_err() {
                fstart_log::error!("{} bootblock failed", B::Platform::NAME);
            }
            fstart_arch::x86_64::halt()
        }
        #[cfg(fstart_stage_env = "postcar")]
        {
            // Teardown already done by the entry; load the ramstage cached.
            let Ok(spec) = bootstrap_spec::<B>(1) else {
                fstart_arch::x86_64::halt()
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

/// Mainboard hooks contribute ACPI fragments through the same [`AcpiDevice`]
/// abstraction chipset drivers use. Vacuous when ACPI is disabled.
///
/// [`AcpiDevice`]: fstart_acpi::device::AcpiDevice
#[cfg(all(feature = "stage", feature = "acpi"))]
pub trait MainboardAcpi<P: IntelEarlyPlatform>:
    fstart_acpi::device::AcpiDevice<Config = P::AcpiContext>
{
}
#[cfg(all(feature = "stage", feature = "acpi"))]
impl<P, T> MainboardAcpi<P> for T
where
    P: IntelEarlyPlatform,
    T: fstart_acpi::device::AcpiDevice<Config = P::AcpiContext>,
{
}
#[cfg(all(feature = "stage", not(feature = "acpi")))]
pub trait MainboardAcpi<P> {}
#[cfg(all(feature = "stage", not(feature = "acpi")))]
impl<P, T> MainboardAcpi<P> for T {}

/// Mutable context passed to board hooks.
#[cfg(feature = "stage")]
pub struct IntelEarlyCtx<'a, P: IntelEarlyPlatform> {
    southbridge: &'a mut P::Southbridge,
}

#[cfg(feature = "stage")]
impl<'a, P: IntelEarlyPlatform> IntelEarlyCtx<'a, P> {
    #[cfg(any(fstart_stage_env = "car", fstart_stage_env = "ram"))]
    pub(crate) fn new(southbridge: &'a mut P::Southbridge) -> Self {
        Self { southbridge }
    }

    #[must_use]
    pub fn southbridge(&mut self) -> &mut P::Southbridge {
        self.southbridge
    }
}

/// Board hooks at the fixed Intel early-flow seams.
#[cfg(feature = "stage")]
pub trait IntelEarlyBoardHooks<P: IntelEarlyPlatform>: MainboardAcpi<P> {
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
