//! Shared stage-runtime types for the fstart firmware framework.
//!
//! This crate contains the `Board` trait implemented by generated board
//! adapters plus small scalar helper types shared by generated stage code.
//!
//! There is exactly one production stage creation path: `fstart-codegen` emits
//! a direct per-stage `fstart_main()` sequence from the board RON capability
//! list.  The generated sequence calls the `Board` adapter methods directly;
//! there is no alternate plan interpreter in this crate.
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

pub mod mask;

pub use mask::DeviceMask;

use fstart_services::device::DeviceError;
use fstart_types::DeviceId;

// ---------------------------------------------------------------------------
// BootMediaState — runtime record of which boot medium is currently active
// ---------------------------------------------------------------------------

/// Board-adapter bookkeeping for the current boot medium.
///
/// The executor tells the adapter *which* boot medium should be active
/// (via [`Board::boot_media_static`] or [`Board::boot_media_select`])
/// but does not care *how* the adapter represents it.  This enum is
/// the common storage shape the generated adapter uses on `self`:
///
/// - [`None`](Self::None): no boot medium configured yet.  This is the
///   initial state of a fresh adapter, and also the state of a stage
///   whose capability list never touches FFS.
///
/// - [`Mmio`](Self::Mmio): a memory-mapped flash window.  The generated
///   [`Board::sig_verify`] / [`Board::payload_load`] / etc. trampolines
///   reconstruct a [`fstart_services::boot_media::MemoryMapped`]
///   from `base` + `size` on each call.  Cheap — `MemoryMapped` is a
///   `Copy`-ish descriptor with no owned state.
///
/// - [`Block`](Self::Block): a block device (SPI NOR, SD/MMC, …)
///   identified by its `DeviceId`.  The generated trampolines match
///   on the id to pick the right `self.<name>.as_ref().unwrap()` and
///   wrap it in a
///   [`fstart_services::boot_media::BlockDeviceMedia`].
///
/// Using a single state type (rather than trait objects or an
/// adapter-local enum per board) keeps codegen simple and lets
/// [`Board::boot_media_static`] stay `fn(Option<DeviceId>, u64, u64)`
/// without any board-specific argument plumbing.
///
/// # Why this lives in the runtime crate
///
/// The executor does not consume `BootMediaState` directly, but every
/// generated board adapter needs the same discriminants.  Centralising
/// the enum here means:
///
/// - The three variants cannot drift between boards.
/// - Future changes (e.g. adding a `Cached` variant for the
///   RAM-copy-then-read path) land in exactly one place, visible to
///   both `fstart-codegen` and tests.
/// - The invariant that boot-media state is just scalar data (no
///   references, no lifetimes) is encoded in the type itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootMediaState {
    /// No boot medium selected yet, or the stage never selects one.
    None,
    /// Memory-mapped flash at `base..base + size`.  Covers the
    /// [`BootMedium::MemoryMapped`](fstart_types::BootMedium::MemoryMapped)
    /// RON variant.
    Mmio {
        /// CPU-visible base address of the flash window.
        base: u64,
        /// Window size in bytes.
        size: u64,
    },
    /// Block device identified by [`DeviceId`] — SPI, SD/MMC, etc.
    ///
    /// `offset` is where the FFS image begins on the device, `size`
    /// is the image extent.  Covers [`BootMedium::Device`] and the
    /// runtime-selected [`BootMedium::AutoDevice`].
    ///
    /// [`BootMedium::Device`]: fstart_types::BootMedium::Device
    /// [`BootMedium::AutoDevice`]: fstart_types::BootMedium::AutoDevice
    Block {
        /// Which device in the adapter's field set provides the
        /// backing `BlockDevice` impl.
        device_id: DeviceId,
        /// FFS image offset on the device.
        offset: u64,
        /// FFS image size in bytes.
        size: u64,
    },
}

impl BootMediaState {
    /// Compact constructor matching the [`Board::boot_media_static`]
    /// argument shape: `None` selects memory-mapped at `(offset, size)`,
    /// `Some(id)` selects the named block device.
    ///
    /// Kept as a separate function rather than a trait method so host
    /// tests can build expected states without needing a full `Board`
    /// impl.
    #[inline]
    pub const fn from_static(device: Option<DeviceId>, offset: u64, size: u64) -> Self {
        match device {
            None => Self::Mmio { base: offset, size },
            Some(id) => Self::Block {
                device_id: id,
                offset,
                size,
            },
        }
    }
}

/// One candidate in a runtime-selected boot-media table.
///
/// Generated direct codeflow emits static slices of this type for
/// `BootMedia(AutoDevice)` and `LoadNextStage` operations, then passes them to
/// [`Board::boot_media_select`].
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

/// The complete surface generated direct codeflow uses to drive a
/// board-specific `Devices` struct.
///
/// Implemented **once per board, by codegen.** Every device-bearing
/// method takes a [`DeviceId`] and the impl dispatches to a concrete
/// field via an inline match.  Because the trait is `Sized` and direct codeflow uses fully qualified trait
/// calls, builds produce specialised
/// code with no vtables and no dynamic dispatch.
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

    /// For every enabled non-structural device in the board, call
    /// `init_device` unless either:
    ///
    /// - the device is already present in `skip`, or
    /// - the device is present in `gated` **and** the currently
    ///   selected boot medium doesn't match the device's
    ///   `boot_media_ids`.
    ///
    /// Subsumes `generate_driver_init` from `fstart-codegen`.
    fn init_all_devices(&mut self, skip: &DeviceMask, gated: &DeviceMask);

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

    /// Stage operation for `LateDriverInit`.  `count` is currently always `0`.
    ///
    /// Generated adapter delegates to
    /// `fstart_capabilities::late_driver_init_complete`.
    fn late_driver_init_complete(&mut self, count: usize);

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

    /// Selection step for `BootMedia(AutoDevice)` / `LoadNextStage`.  Inspects the hardware boot-source register and
    /// picks one of `candidates`, recording the selection inside the
    /// board so later `sig_verify` / `payload_load` / etc. read from
    /// the right place.
    ///
    /// Returns the chosen candidate's `DeviceId`, or `None` if
    /// nothing matched.
    fn boot_media_select(&mut self, candidates: &[BootMediaCandidate]) -> Option<DeviceId>;

    /// Stage operation for static boot media. Records the
    /// static boot-media descriptor inside the board so later
    /// capabilities (`sig_verify`, etc.) read from it.
    ///
    /// - `device = None`: memory-mapped flash at `(offset, size)`.
    /// - `device = Some(id)`: block device `id` starting at `offset`,
    ///   `size` bytes long.
    fn boot_media_static(&mut self, device: Option<DeviceId>, offset: u64, size: u64);

    /// Stage operation for `LoadNextStage`.  Diverges.  Uses
    /// whichever boot medium `boot_media_select` just picked to read
    /// the named next stage and jump to it.
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
