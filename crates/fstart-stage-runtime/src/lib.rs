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

#[cfg(feature = "stage-executor")]
pub mod flow;
pub mod mask;
pub mod plan;

#[cfg(feature = "stage-executor")]
pub use flow::run_stage;
pub use mask::DeviceMask;
pub use plan::{StageOp, StagePlan};

use fstart_services::device::DeviceError;
use fstart_services::FirmwareImage;
use fstart_types::{DeviceId, TempRamBuffer};

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
/// - **Diverging trampolines return `!`.**  `payload_load`,
///   `stage_load`, `load_next_stage`, `return_to_fel` never come back.
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

    // ----- Capability trampolines -----------------------------------------
    //
    // Each method below corresponds to one generated stage operation.  The generated
    // board adapter implements every method as a single line delegating
    // to `fstart_capabilities::*`, plus any state the capability needs
    // (addresses, descriptors) read from `&self` fields.
    //
    // Keeping trampolines on the trait — rather than making
    // `fstart-stage-runtime` depend on `fstart-capabilities` — avoids a
    // dep cycle with `fstart-log` and keeps the runtime free of the
    // FFS / crypto / FDT / SMBIOS tree.

    /// Stage operation for `MemoryInit`.
    ///
    /// Generated adapter delegates to `fstart_capabilities::memory_init`.
    fn memory_init(&self);

    /// Stage operation for `SigVerify`.
    ///
    /// Generated adapter reads its anchor pointer and current boot
    /// media from `&self` and calls `fstart_capabilities::sig_verify`.
    fn sig_verify(&self);

    /// Stage operation for `FdtPrepare`.
    ///
    /// Generated adapter reads DTB source/destination addresses,
    /// bootargs, DRAM base, and DRAM size (from a previously handed-off
    /// memory map if present, else a compile-time constant) from
    /// `&self`, and calls `fstart_capabilities::fdt_prepare_platform`.
    fn fdt_prepare(&self);

    /// Stage operation for `PayloadLoad`.  Diverges.
    ///
    /// Generated adapter reads its anchor + boot media from `&self`
    /// and calls `fstart_capabilities::payload_load`.  Halts on
    /// failure.
    fn payload_load(&self) -> !;

    /// Stage operation for `StageLoad`.  Diverges.  `next_stage` comes from
    /// the board RON capability.
    ///
    /// Generated adapter reads its anchor + boot media from `&self`
    /// and calls `fstart_capabilities::stage_load`.  Halts on failure.
    fn stage_load(&self, next_stage: &str) -> !;

    /// Stage operation for `AcpiPrepare`.
    ///
    /// Generated adapter calls `fstart_capabilities::acpi_prepare`
    /// with its AcpiConfig descriptor (held in `&self`).
    fn acpi_prepare(&mut self);

    /// Stage operation for `SmBiosPrepare`.
    ///
    /// Generated adapter calls `fstart_capabilities::smbios_prepare`
    /// with its SmbiosConfig descriptor (held in `&self`).
    fn smbios_prepare(&self);

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

    /// Stage operation for `PreConsoleInit`.
    fn pre_console_init(&mut self, ids: &[DeviceId]) -> Result<(), DeviceError> {
        let _ = ids;
        Ok(())
    }

    /// Stage operation for `EarlyInit`.
    fn early_init(&mut self, ids: &[DeviceId]) -> Result<(), DeviceError> {
        let _ = ids;
        Ok(())
    }

    /// Stage operation for `StageLocalInit`.
    fn stage_local_init(&mut self, ids: &[DeviceId]) -> Result<(), DeviceError> {
        let _ = ids;
        Ok(())
    }

    /// Stage operation for `PostDramInit`.
    fn post_dram_init(&mut self, ids: &[DeviceId]) -> Result<(), DeviceError> {
        let _ = ids;
        Ok(())
    }

    /// Stage operation for `FinalizeInit`.
    fn finalize_init(&mut self, ids: &[DeviceId]) -> Result<(), DeviceError> {
        let _ = ids;
        Ok(())
    }

    /// Stage operation for `DramInit`. `id` is the resolved device ID.
    ///
    /// Generated adapter dispatches to `MemoryController::dram_init()` on the
    /// already-constructed controller. This deliberately does not go through
    /// only `init_device()`: memory controllers may already be constructed by
    /// a `PreConsoleInit` phase, while DRAM training must happen later after
    /// chipset/SMBus setup.
    fn dram_init(&mut self, id: DeviceId) -> Result<(), DeviceError>;

    /// Stage operation for `PciInit`.  `id` is the resolved device ID.
    ///
    /// Generated adapter calls the appropriate `PciHost::enumerate`
    /// plus `allocate_windows`.
    fn pci_init(&mut self, id: DeviceId) -> Result<(), DeviceError>;

    /// Stage operation for `AcpiLoad`.  `id` is the resolved device ID.
    ///
    /// Generated adapter reads the ACPI target buffer from `&self`
    /// and calls `fstart_capabilities::acpi_load`.
    fn acpi_load(&mut self, id: DeviceId) -> Result<(), DeviceError>;

    /// Stage operation for `MemoryDetect`.  `id` is the resolved device ID.
    ///
    /// Generated adapter reads the target memory-map buffer from
    /// `&self` and calls `fstart_capabilities::memory_detect`.
    fn memory_detect(&mut self, id: DeviceId) -> Result<(), DeviceError>;

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

    /// Record a firmware image backed by a Rust platform-selected block device
    /// so later capabilities (`sig_verify`, etc.) read from it.
    fn boot_media_block_firmware_image(
        &mut self,
        device: DeviceId,
        offset: u64,
        size: u64,
        temp_ram_buffer: Option<TempRamBuffer>,
    );

    /// Read a firmware-image descriptor from a provider device.
    ///
    /// `provider` names a device implementing `FirmwareImageProvider`. The
    /// stage executor owns how that descriptor becomes active boot-media state.
    fn firmware_image(&self, provider: DeviceId) -> Result<FirmwareImage, RuntimeError>;

    /// Stage operation for Rust platform-backed firmware-image boot media.
    ///
    /// This covers fixed emulator/SoC ROM windows whose mapping is known from
    /// platform code rather than a runtime device register.
    fn boot_media_platform_firmware_image(
        &mut self,
        image: FirmwareImage,
        temp_ram_buffer: Option<TempRamBuffer>,
    ) -> Result<(), RuntimeError>;

    /// Stage operation for `LoadNextStage`. Diverges. Uses whichever boot
    /// medium the executor published to read the named next stage and jump to
    /// it.
    fn load_next_stage(&mut self, next_stage: &str) -> !;

    // ----- Platform primitives --------------------------------------------

    /// Stop forever.  Generated adapter delegates to
    /// `fstart_platform::halt`.
    fn halt(&self) -> !;

    /// Jump to `entry` in RAM.  Generated adapter delegates to
    /// `fstart_platform::jump_to`.  Platform-specific argument
    /// registers (hart id, DTB pointer, handoff) are set up inside
    /// the platform crate's entry assembly.
    fn jump_to(&self, entry: u64) -> !;

    /// Jump to `entry` passing a serialised handoff descriptor to the
    /// next stage.  Generated adapter delegates to
    /// `fstart_platform::jump_to_with_handoff`.
    fn jump_to_with_handoff(&self, entry: u64, handoff_addr: usize) -> !;
}
