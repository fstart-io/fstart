//! Shared stage-runtime types for the fstart firmware framework.
//!
//! This crate contains the `Board` trait implemented by generated board
//! adapters plus small scalar helper types shared by generated stage code.
//!
//! There is exactly one production stage creation path: `fstart-codegen` emits
//! data-only `StagePlan` tables plus a small `fstart_main()` shim into
//! [`run_stage`]. The executor runs handwritten Rust flow over those facts.
//!
//! # Multi-platform constraints
//!
//! The trait shape deliberately keeps all board-level data — addresses,
//! bootargs, anchor pointer, DRAM sizes, flash bases — out of method
//! arguments.  Trampolines read everything they need from `&self`.
//! This means a future multi-platform codegen can produce `Devices`
//! structs that carry variant-per-platform fields without changing the
//! trait.

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(feature = "flow-acpi")]
extern crate alloc;

#[cfg(feature = "stage-executor")]
pub mod flow;
pub mod mask;
pub mod plan;

#[cfg(feature = "stage-executor")]
pub use flow::run_stage;
pub use mask::DeviceMask;
pub use plan::{StageOp, StagePlan};

use fstart_services::device::DeviceError;
use fstart_services::{BootMedia, FirmwareImage, TempRamArena};
use fstart_types::{DeviceId, TempRamBuffer};

/// Mutable DSDT AML buffer passed from the ACPI executor to board-local
/// device table collection.
#[cfg(feature = "flow-acpi")]
pub type AcpiDsdtAml = alloc::vec::Vec<u8>;

/// Mutable list of standalone ACPI tables collected from board devices.
#[cfg(feature = "flow-acpi")]
pub type AcpiExtraTables = alloc::vec::Vec<alloc::vec::Vec<u8>>;

/// Source policy for `FdtPrepare` expressed as data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FdtPrepareSource {
    /// No real FDT source; run the stub capability.
    Stub,
    /// Patch a platform-provided FDT at the given source address.
    Platform { src_dtb_addr: u64 },
    /// Load an override DTB from FFS, then patch it in place at the
    /// descriptor's destination address.
    Override,
}

/// Primitive FDT preparation descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FdtPrepareDesc {
    /// Source mode for this stage.
    pub source: FdtPrepareSource,
    /// Destination DTB address.
    pub dst_dtb_addr: u64,
    /// Kernel command line to write to `/chosen/bootargs`.
    pub bootargs: &'static str,
    /// DRAM base address for `/memory` patching.
    pub dram_base: u64,
    /// DRAM size for `/memory` patching.
    pub dram_size: u64,
}

/// Payload handoff policy expressed as data for the runtime executor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadLoadKind {
    /// Generic FFS payload: load the `Payload` file and jump to its entry.
    GenericFfs,
    /// LinuxBoot or FIT-buildtime: load optional firmware plus the kernel from
    /// FFS, then enter the platform Linux boot protocol.
    LinuxBoot,
    /// FIT-runtime: load the FIT blob from FFS, parse it at runtime, then enter
    /// the platform Linux boot protocol at the FIT-provided kernel address.
    FitRuntime {
        /// Optional FIT configuration name.
        config: Option<&'static str>,
    },
    /// UEFI/CrabEFI payload.  This path still needs platform-service
    /// primitives and is delegated to a narrower board method for now.
    Uefi,
}

/// Primitive payload-load descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PayloadLoadDesc {
    /// Payload loading strategy.
    pub kind: PayloadLoadKind,
    /// Statically configured kernel load address for non-FIT-runtime Linux.
    pub kernel_addr: u64,
    /// DTB address for the platform Linux boot protocol.
    pub dtb_addr: u64,
    /// Optional firmware entry/load address for the platform Linux boot protocol.
    pub fw_addr: u64,
    /// Kernel command line.
    pub bootargs: &'static str,
    /// Whether a firmware blob should be loaded from FFS before handoff.
    pub load_firmware: bool,
    /// x86-only diagnostic flag forwarded to the platform Linux boot protocol.
    pub print_x86_mtrrs: bool,
}

/// Primitive descriptor for `StageLoad`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StageLoadDesc {
    /// Whether this stage must use the x86 post-CAR MMIO handoff path.
    pub x86_postcar: bool,
}

/// Load and handoff addresses for a named next stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NextStageAddr {
    /// Entry/load address for the next stage.
    pub load_addr: u64,
    /// Address of the serialized handoff buffer.
    pub handoff_addr: u64,
}

/// eGON-published next-stage extent inside the selected firmware image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EgonNextStage {
    /// Offset of the next-stage image relative to the firmware-image base.
    pub offset: u64,
    /// Size of the next-stage image in bytes.
    pub size: usize,
}

// ---------------------------------------------------------------------------
// BootMediaState — runtime record of which boot medium is currently active
// ---------------------------------------------------------------------------

/// Board-adapter bookkeeping for the current boot medium.
///
/// The executor tells the adapter *which* boot medium should be active
/// (via provider-backed firmware-image setup or explicit state publication)
/// but does not care *how* the adapter represents it.  This enum is
/// the common storage shape the generated adapter uses on `self`:
///
/// - [`None`](Self::None): no boot medium configured yet.  This is the
///   initial state of a fresh adapter, and also the state of a stage
///   whose capability list never touches FFS.
///
/// - [`FirmwareImage`](Self::FirmwareImage): a firmware image supplied by a
///   Rust provider/platform memory mapping.
///
/// - [`FirmwareImageBlock`](Self::FirmwareImageBlock): a firmware image whose
///   backing storage is reached through a Rust platform-selected block device
///   (for example sunxi eGON boot-source selection). The generated trampolines
///   match on the device id to pick the right `self.<name>.as_ref().unwrap()`
///   and wrap it in [`fstart_services::boot_media::BlockDeviceMedia`].
///
/// Using a single state type (rather than trait objects or an
/// adapter-local enum per board) keeps codegen simple without any
/// board-specific argument plumbing.
///
/// # Why this lives in the runtime crate
///
/// The executor does not consume `BootMediaState` directly, but every
/// generated board adapter needs the same discriminants.  Centralising
/// the enum here means:
///
/// - The three variants cannot drift between boards.
/// - Future changes (e.g. adding a cached/scratch-backed variant) land in
///   exactly one place, visible to
///   both `fstart-codegen` and tests.
/// - The invariant that boot-media state is just scalar data (no
///   references, no lifetimes) is encoded in the type itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootMediaState {
    /// No boot medium selected yet, or the stage never selects one.
    None,
    /// Firmware image mapping supplied by a hardware provider.
    FirmwareImage {
        /// Logical firmware image and its CPU-visible windows.
        image: FirmwareImage,
        /// Optional scratch arena for temporary FFS/payload buffers.
        temp_ram_buffer: Option<TempRamBuffer>,
    },
    /// Firmware image backed by a platform-selected block device.
    ///
    /// `offset` is where the FFS image begins on the device, `size`
    /// is the image extent. This covers runtime-selected Rust platform
    /// boot-source candidates while keeping the public boot-media model as
    /// `FirmwareImage`.
    FirmwareImageBlock {
        /// Which device in the adapter's field set provides the
        /// backing `BlockDevice` impl.
        device_id: DeviceId,
        /// FFS image offset on the device.
        offset: u64,
        /// FFS image size in bytes.
        size: u64,
        /// Optional scratch arena for temporary FFS/payload buffers.
        temp_ram_buffer: Option<TempRamBuffer>,
    },
}

impl BootMediaState {
    /// Construct a block-device-backed firmware-image state.
    #[inline]
    pub const fn from_block_firmware_image(
        device_id: DeviceId,
        offset: u64,
        size: u64,
        temp_ram_buffer: Option<TempRamBuffer>,
    ) -> Self {
        Self::FirmwareImageBlock {
            device_id,
            offset,
            size,
            temp_ram_buffer,
        }
    }

    /// Construct a firmware-image-backed boot media state.
    #[inline]
    pub const fn from_firmware_image(
        image: FirmwareImage,
        temp_ram_buffer: Option<TempRamBuffer>,
    ) -> Self {
        Self::FirmwareImage {
            image,
            temp_ram_buffer,
        }
    }
}

/// One candidate in a runtime-selected boot-media table.
///
/// Generated `StagePlan` data uses static slices of this type for
/// provider/platform boot media and `LoadNextStage` operations, then the
/// executor matches them against [`Board::soc_boot_media`].
#[derive(Debug, Clone, Copy)]
pub struct BootMediaCandidate {
    /// Device to use as the boot medium.
    pub device: DeviceId,
    /// Offset into the device where the firmware image starts.
    pub offset: u64,
    /// Size of the firmware image on the device.
    ///
    /// `LoadNextStage` candidates use `0`; the stage loader reads the exact
    /// next-stage size from the platform-specific image header.
    pub size: u64,
    /// Hardware boot-source register values that select this candidate.
    pub media_ids: &'static [u8],
}

/// Error type for runtime-data provider methods on [`Board`].
///
/// A deliberate placeholder rather than `Result<_, ()>` so variants
/// can be added without churning every impl.  The generated board
/// adapter collapses richer driver errors into one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeError {
    /// The board adapter was asked to service an id it didn't know
    /// about.  Usually indicates a codegen bug.
    UnknownDevice,
    /// The underlying hardware or firmware operation failed.  The
    /// board impl may have logged more detail.
    Failed,
    /// A provided buffer was too small to hold the result.
    BufferTooSmall,
}

/// Generic device lifecycle phase selected by the handwritten executor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StagePhase {
    /// `PreConsoleInit` provider hook.
    PreConsoleInit,
    /// `EarlyInit` provider hook.
    EarlyInit,
    /// `StageLocalInit` provider hook.
    StageLocalInit,
    /// `PostDramInit` provider hook.
    PostDramInit,
    /// `FinalizeInit` provider hook.
    FinalizeInit,
}

// ---------------------------------------------------------------------------
// Board trait
// ---------------------------------------------------------------------------

/// The complete surface the stage executor uses to drive a board-specific
/// `Devices` struct.
///
/// Implemented **once per board, by codegen.** Every device-bearing
/// method takes a [`DeviceId`] and the impl dispatches to a concrete
/// field via an inline match. Because the trait is `Sized` and the executor is
/// generic over `B: Board`, builds produce specialised code with no vtables and
/// no dynamic dispatch.
///
/// # Trait design rules (see plan doc §Invariants)
///
/// - **Method bodies read everything from `&self`.**  No board-level
///   constant is ever a method argument.  This is what keeps
///   multi-platform binaries viable later: the adapter carries the
///   per-platform data in fields, and the trait signature does not
///   change when the adapter grows variant-per-platform fields.
///
/// - **Codeflow-derived context only.**  Arguments come from resolved stage
///   metadata (`DeviceId`, `next_stage`) rather than board-level addresses,
///   sizes, bootargs, or descriptor strings.
///
/// - **Diverging platform endpoints return `!`.**  Handwritten runtime flow
///   owns payload/stage loading and next-stage sequencing; final platform
///   endpoints such as `uefi_payload_load`, `return_to_fel`, `boot_linux`,
///   `jump_to`, and `jump_to_with_handoff` never come back.
pub trait Board: Sized {
    // ----- Device lifecycle ------------------------------------------------

    /// Construct `id` (and any not-yet-constructed non-structural
    /// ancestors) and call `Device::init` / `BusDevice::init` on each
    /// in root-first order.  Idempotent: repeated calls for the same
    /// `id` are no-ops.
    ///
    /// Subsumes `ensure_device_ready`, `walk_to_real_parent`, and
    /// `generate_device_construction` from `fstart-codegen`.
    fn init_device(&mut self, id: DeviceId) -> Result<(), DeviceError>;

    // ----- Logging --------------------------------------------------------

    /// Install the global `fstart_log` logger on the console device
    /// with id `id`.  Must be called after `init_device(id)` and
    /// before any `fstart_log::*!` macro runs.
    ///
    /// # Safety
    ///
    /// `id` must be a device that provides `Console` and that was
    /// already constructed by `init_device(id)`.  The board impl
    /// promises to hold the device for the stage's lifetime, which
    /// justifies extending the borrow to `'static` inside.
    unsafe fn install_logger(&self, id: DeviceId);

    // ----- Remaining high-level operations and primitive accessors --------
    //
    // This trait is being narrowed toward primitive board access.  Handwritten
    // runtime flow owns operation sequencing and capability calls; generated
    // adapters should expose only concrete device dispatch, static descriptors,
    // and small scalar state.  A few older high-level methods remain below and
    // are intentionally visible as the next refactor targets.

    /// Static/handoff-derived FDT preparation descriptor, if this stage has one.
    ///
    /// The executor owns FFS override loading and the FDT capability call; the
    /// board adapter only exposes source addresses and patch metadata.
    #[cfg(feature = "flow-fdt")]
    fn fdt_prepare_desc(&self) -> Option<FdtPrepareDesc>;

    /// Static payload-load descriptor, if this stage has one.
    ///
    /// The executor owns FFS/FIT loading and Linux handoff sequencing. UEFI is
    /// still delegated through [`Board::uefi_payload_load`] until its platform
    /// service access is reduced further.
    #[cfg(feature = "flow-ffs")]
    fn payload_load_desc(&self) -> Option<PayloadLoadDesc>;

    /// Remaining UEFI/CrabEFI payload path. Diverges.
    #[cfg(feature = "flow-ffs")]
    fn uefi_payload_load(&self) -> !;

    /// Static `StageLoad` descriptor, if this stage has one.
    #[cfg(feature = "flow-ffs")]
    fn stage_load_desc(&self) -> Option<StageLoadDesc>;

    /// Minimal x86 post-CAR stage-load platform primitive.
    ///
    /// The executor owns selecting the active firmware-image window and error
    /// policy; the board adapter supplies only the platform post-CAR data and
    /// invokes the platform's stack-switching handoff primitive.
    #[cfg(feature = "flow-ffs")]
    fn stage_load_postcar_mmio(
        &self,
        next_stage: &str,
        anchor: &'static [u8],
        image_base: u64,
        image_size: u64,
    ) -> !;

    /// Static platform ACPI descriptor for `AcpiPrepare`, if this stage has one.
    #[cfg(feature = "flow-acpi")]
    fn acpi_platform_config(&self) -> Option<(fstart_acpi::platform::PlatformConfig, bool)>;

    /// Collect board/device AML and standalone ACPI tables.
    ///
    /// The executor owns ACPI table allocation and capability invocation; the
    /// board adapter only dispatches to concrete `AcpiDevice` implementations
    /// and appends their data to the caller-owned buffers.
    #[cfg(feature = "flow-acpi")]
    fn collect_acpi_tables(
        &self,
        dsdt_aml: &mut AcpiDsdtAml,
        extra_tables: &mut AcpiExtraTables,
    ) -> Result<(), RuntimeError>;

    /// Static SMBIOS descriptor for `SmBiosPrepare`, if this stage has one.
    #[cfg(feature = "flow-smbios")]
    fn smbios_desc(&self) -> Option<fstart_capabilities::smbios::SmbiosDesc<'static>>;

    /// Stage operation for `MpInit`.
    ///
    /// Board adapters that enable MP/SMM construct the concrete CPU and
    /// platform SMM operations here and delegate to `fstart_mp::mp_init`.
    /// The default no-op preserves existing non-x86 board adapters until
    /// their codegen grows a real implementation.
    fn mp_init(&mut self, cpu_model: &str, num_cpus: u16, smm: bool) -> Result<(), RuntimeError> {
        let _ = (cpu_model, num_cpus, smm);
        Ok(())
    }

    /// Run one device-local lifecycle phase hook.
    ///
    /// The executor owns phase iteration and init-before-callback policy; the
    /// board adapter only dispatches a `(phase, id)` pair to the concrete
    /// service implementation.
    fn phase_init(&mut self, phase: StagePhase, id: DeviceId) -> Result<(), DeviceError>;

    /// Stage operation for `DramInit`. `id` is the resolved device ID.
    ///
    /// Generated adapter dispatches to `MemoryController::dram_init()` on the
    /// already-constructed controller. This deliberately does not go through
    /// only `init_device()`: memory controllers may already be constructed by
    /// a `PreConsoleInit` phase, while DRAM training must happen later after
    /// chipset/SMBus setup.
    fn dram_init(&mut self, id: DeviceId) -> Result<(), DeviceError>;

    /// Borrow a PCI root bus by device id for one operation.
    ///
    /// The executor owns init policy and service invocation; the board adapter
    /// only dispatches to the concrete provider field and supplies names for
    /// logging.
    #[cfg(feature = "flow-pci")]
    fn with_pci_root<R>(
        &mut self,
        id: DeviceId,
        run: impl FnOnce(&mut dyn fstart_services::PciRootBus, &'static str, &'static str) -> R,
    ) -> Result<R, RuntimeError>;

    /// Borrow an ACPI table provider by device id for one operation.
    ///
    /// The executor owns buffer allocation, capability invocation, and RSDP
    /// state publication; the board adapter only dispatches to the concrete
    /// provider field and supplies the static device name for logging.
    fn with_acpi_table_provider<R>(
        &self,
        id: DeviceId,
        run: impl FnOnce(&dyn fstart_services::acpi_provider::AcpiTableProvider, &'static str) -> R,
    ) -> Result<R, RuntimeError>;

    /// Publish the prepared ACPI RSDP address.
    fn set_acpi_rsdp_addr(&mut self, addr: u64);

    /// Borrow a memory detector by device id for one operation.
    ///
    /// The executor owns buffer allocation and capability invocation; the
    /// board adapter only dispatches to the concrete provider field and
    /// supplies the static device name for logging.
    fn with_memory_detector<R>(
        &self,
        id: DeviceId,
        run: impl FnOnce(&dyn fstart_services::memory_detect::MemoryDetector, &'static str) -> R,
    ) -> Result<R, RuntimeError>;

    /// Stage operation for `ReturnToFel`.  Diverges.  Armv7
    /// sunxi-only; on other platforms the generated adapter emits
    /// `unreachable!()`.
    fn return_to_fel(&self) -> !;

    // ----- Boot media selection -------------------------------------------

    /// Read the platform boot-media selector, if this board has one.
    ///
    /// The stage executor owns candidate matching and state transitions; the
    /// board adapter only exposes the primitive boot-source byte.
    fn soc_boot_media(&self) -> Option<u8>;

    /// Publish the active boot-media state selected by the executor.
    fn set_boot_media_state(&mut self, state: BootMediaState);

    /// Read a firmware-image descriptor from a provider device.
    ///
    /// `provider` names a device implementing `FirmwareImageProvider`. The
    /// stage executor owns how that descriptor becomes active boot-media state.
    fn firmware_image(&self, provider: DeviceId) -> Result<FirmwareImage, RuntimeError>;

    /// FFS anchor bytes for stages that use the firmware filesystem.
    fn ffs_anchor(&self) -> Option<&'static [u8]>;

    /// Active contiguous memory-mapped firmware window, if selected.
    #[cfg(feature = "flow-ffs")]
    fn active_firmware_window(&self) -> Option<(u64, u64)>;

    /// Run a closure with the active boot medium and optional scratch arena.
    ///
    /// The executor owns the FFS/payload operation; the board adapter only
    /// resolves the current scalar [`BootMediaState`] into a concrete
    /// [`BootMedia`] implementation and block-device field borrow.
    fn with_boot_media<R>(
        &self,
        caller_tag: &str,
        none: R,
        run: impl FnOnce(&dyn BootMedia, Option<&mut TempRamArena>) -> R,
    ) -> R;

    /// Borrow a block device by device id for one operation.
    fn with_block_device<R>(
        &self,
        id: DeviceId,
        run: impl FnOnce(&dyn fstart_services::BlockDevice, &'static str) -> R,
    ) -> Result<R, RuntimeError>;

    /// Resolve a named next-stage load/handoff address pair.
    #[cfg(feature = "flow-fel")]
    fn next_stage_addr(&self, next_stage: &str) -> Option<NextStageAddr>;

    /// Read eGON-published next-stage offset and size scalars.
    #[cfg(feature = "flow-fel")]
    fn egon_next_stage(&self) -> Option<EgonNextStage>;

    /// DRAM size to serialize into the next-stage handoff.
    #[cfg(feature = "flow-fel")]
    fn dram_size_for_handoff(&self) -> u64;

    // ----- Platform primitives --------------------------------------------

    /// Stop forever.  Generated adapter delegates to
    /// `fstart_platform::halt`.
    fn halt(&self) -> !;

    /// Jump to `entry` in RAM.  Generated adapter delegates to
    /// `fstart_platform::jump_to`.  Platform-specific argument
    /// registers (hart id, DTB pointer, handoff) are set up inside
    /// the platform crate's entry assembly.
    fn jump_to(&self, entry: u64) -> !;

    /// Return the platform boot hart id, or 0 on non-RISC-V platforms.
    fn boot_hart_id(&self) -> u64;

    /// Return the currently prepared ACPI RSDP address, or 0 if absent.
    fn acpi_rsdp_addr(&self) -> u64;

    /// Enter the platform Linux boot protocol. Diverges.
    fn boot_linux(&self, params: &fstart_services::boot::BootLinuxParams<'_>) -> !;

    /// Jump to `entry` passing a serialised handoff descriptor to the
    /// next stage.  Generated adapter delegates to
    /// `fstart_platform::jump_to_with_handoff`.
    fn jump_to_with_handoff(&self, entry: u64, handoff_addr: usize) -> !;
}
