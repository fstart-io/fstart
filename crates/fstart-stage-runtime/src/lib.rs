//! Handwritten stage executor for the fstart firmware framework.
//!
//! This crate implements the **runtime half** of the stage/codegen
//! split described in `.opencode/plans/stage-runtime-codegen-split.md`.
//! It pairs with `fstart-codegen` (the build-time half):
//!
//! - `fstart-codegen` emits, for each board, an `impl Board for Devices`
//!   and a `static STAGE_PLAN: StagePlan` in `.rodata`.
//! - `fstart-stage-runtime` provides the generic [`run_stage`] executor
//!   that consumes those two artifacts.
//!
//! In Rigid mode the executor monomorphises on `B: Board`, so every
//! call inlines — the compiled firmware is the same shape as what
//! today's full-codegen approach produces, just authored very
//! differently on the host side.
//!
//! # Current status
//!
//! The trait and executor carry the full final shape (all capabilities
//! dispatched through trampolines that read board state from `&self`).
//! The codegen that emits `impl Board for Devices` lands in a later
//! step; until then the executor is exercised only by host tests using
//! the [`tests::MockBoard`] helper, and the existing `generate_fstart_main`
//! in `fstart-codegen` stays in charge of real boards.
//!
//! # Multi-platform constraints
//!
//! The trait shape deliberately keeps all board-level data — addresses,
//! bootargs, anchor pointer, DRAM sizes, flash bases — out of method
//! arguments.  Trampolines read everything they need from `&self`.
//! This means a future multi-platform codegen can produce `Devices`
//! structs that carry variant-per-platform fields without changing the
//! trait or the executor.  See
//! `.opencode/plans/stage-runtime-codegen-split.md` §"Invariants that
//! preserve multi-platform extensibility".

#![cfg_attr(not(feature = "std"), no_std)]

pub mod mask;
pub mod plan;

pub use mask::DeviceMask;
pub use plan::{BootMediaCandidate, CapOp, StagePlan};

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

/// The complete surface [`run_stage`] uses to drive a board-specific
/// `Devices` struct.
///
/// Implemented **once per board, by codegen.** Every device-bearing
/// method takes a [`DeviceId`] and the impl dispatches to a concrete
/// field via an inline match.  Because the trait is `Sized` and the
/// executor is generic over `B: Board`, Rigid-mode builds produce
/// specialised code with no vtables and no dynamic dispatch.
///
/// # Trait design rules (see plan doc §Invariants)
///
/// - **Method bodies read everything from `&self`.**  No board-level
///   constant is ever a method argument.  This is what keeps
///   multi-platform binaries viable later: the adapter carries the
///   per-platform data in fields, and the trait signature does not
///   change when the adapter grows variant-per-platform fields.
///
/// - **Executor-derived context only.**  Arguments come from
///   [`CapOp`] variants (`DeviceId`, `next_stage`) or from
///   [`StagePlan`] (`is_first_stage` → `uses_handoff`).  The executor
///   never passes addresses, sizes, bootargs, or descriptor strings.
///
/// - **Diverging trampolines return `!`.**  `payload_load`,
///   `stage_load`, `load_next_stage`, `return_to_fel` never come
///   back; the executor arm is just `board.foo()`.
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
    // Each method below corresponds to one executor arm.  The generated
    // board adapter implements every method as a single line delegating
    // to `fstart_capabilities::*`, plus any state the capability needs
    // (addresses, descriptors) read from `&self` fields.
    //
    // Keeping trampolines on the trait — rather than making
    // `fstart-stage-runtime` depend on `fstart-capabilities` — avoids a
    // dep cycle with `fstart-log` and keeps the runtime free of the
    // FFS / crypto / FDT / SMBIOS tree.

    /// Executor arm for [`CapOp::MemoryInit`].
    ///
    /// Generated adapter delegates to `fstart_capabilities::memory_init`.
    fn memory_init(&self);

    /// Executor arm for [`CapOp::LateDriverInit`].  `count` comes
    /// from the executor's bookkeeping and is currently always `0`.
    ///
    /// Generated adapter delegates to
    /// `fstart_capabilities::late_driver_init_complete`.
    fn late_driver_init_complete(&self, count: usize);

    /// Executor arm for [`CapOp::SigVerify`].
    ///
    /// Generated adapter reads its anchor pointer and current boot
    /// media from `&self` and calls `fstart_capabilities::sig_verify`.
    fn sig_verify(&self);

    /// Executor arm for [`CapOp::FdtPrepare`].
    ///
    /// Generated adapter reads DTB source/destination addresses,
    /// bootargs, DRAM base, and DRAM size (from a previously handed-off
    /// memory map if present, else a compile-time constant) from
    /// `&self`, and calls `fstart_capabilities::fdt_prepare_platform`.
    fn fdt_prepare(&self);

    /// Executor arm for [`CapOp::PayloadLoad`].  Diverges.
    ///
    /// Generated adapter reads its anchor + boot media from `&self`
    /// and calls `fstart_capabilities::payload_load`.  Halts on
    /// failure.
    fn payload_load(&self) -> !;

    /// Executor arm for [`CapOp::StageLoad`].  Diverges.  `next_stage`
    /// comes from the CapOp variant.
    ///
    /// Generated adapter reads its anchor + boot media from `&self`
    /// and calls `fstart_capabilities::stage_load`.  Halts on failure.
    fn stage_load(&self, next_stage: &str) -> !;

    /// Executor arm for [`CapOp::AcpiPrepare`].
    ///
    /// Generated adapter calls `fstart_capabilities::acpi_prepare`
    /// with its AcpiConfig descriptor (held in `&self`).
    fn acpi_prepare(&mut self);

    /// Executor arm for [`CapOp::SmBiosPrepare`].
    ///
    /// Generated adapter calls `fstart_capabilities::smbios_prepare`
    /// with its SmbiosConfig descriptor (held in `&self`).
    fn smbios_prepare(&self);

    /// Executor arm for [`CapOp::ChipsetInit`].  `nb` and `sb` come
    /// from the CapOp variant.
    ///
    /// Generated adapter calls `PciHost::early_init(&self.<nb>)` and
    /// `Southbridge::early_init(&self.<sb>)` (both having been
    /// constructed earlier by `init_device`).
    fn chipset_init(&mut self, nb: DeviceId, sb: DeviceId) -> Result<(), DeviceError>;

    /// Executor arm for [`CapOp::PciInit`].  `id` comes from the
    /// CapOp variant.
    ///
    /// Generated adapter calls the appropriate `PciHost::enumerate`
    /// plus `allocate_windows`.
    fn pci_init(&mut self, id: DeviceId) -> Result<(), DeviceError>;

    /// Executor arm for [`CapOp::AcpiLoad`].  `id` comes from the
    /// CapOp variant.
    ///
    /// Generated adapter reads the ACPI target buffer from `&self`
    /// and calls `fstart_capabilities::acpi_load`.
    fn acpi_load(&mut self, id: DeviceId) -> Result<(), DeviceError>;

    /// Executor arm for [`CapOp::MemoryDetect`].  `id` comes from the
    /// CapOp variant.
    ///
    /// Generated adapter reads the target memory-map buffer from
    /// `&self` and calls `fstart_capabilities::memory_detect`.
    fn memory_detect(&mut self, id: DeviceId) -> Result<(), DeviceError>;

    /// Executor arm for [`CapOp::ReturnToFel`].  Diverges.  Armv7
    /// sunxi-only; on other platforms the generated adapter emits
    /// `unreachable!()`.
    fn return_to_fel(&self) -> !;

    // ----- Boot media selection -------------------------------------------

    /// Executor arm for [`CapOp::BootMediaAuto`] / [`CapOp::LoadNextStage`]
    /// selection step.  Inspects the hardware boot-source register and
    /// picks one of `candidates`, recording the selection inside the
    /// board so later `sig_verify` / `payload_load` / etc. read from
    /// the right place.
    ///
    /// Returns the chosen candidate's `DeviceId`, or `None` if
    /// nothing matched.
    fn boot_media_select(&mut self, candidates: &[BootMediaCandidate]) -> Option<DeviceId>;

    /// Executor arm for [`CapOp::BootMediaStatic`].  Records the
    /// static boot-media descriptor inside the board so later
    /// capabilities (`sig_verify`, etc.) read from it.
    ///
    /// - `device = None`: memory-mapped flash at `(offset, size)`.
    /// - `device = Some(id)`: block device `id` starting at `offset`,
    ///   `size` bytes long.
    fn boot_media_static(&mut self, device: Option<DeviceId>, offset: u64, size: u64);

    /// Executor arm for [`CapOp::LoadNextStage`].  Diverges.  Uses
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

// ---------------------------------------------------------------------------
// run_stage — the handwritten executor
// ---------------------------------------------------------------------------

/// Execute a stage plan against a board adapter.
///
/// The full replacement for the generated `fstart_main()` body.  It
/// consumes exactly two per-board artifacts emitted by codegen:
///
/// 1. The board adapter `board: B: Board` (with its `Devices` field
///    holding the concrete driver instances).
/// 2. The `plan: &'static StagePlan` describing the capability
///    sequence.
///
/// Plus one platform-supplied value:
///
/// 3. `handoff_ptr` — the register the platform's `_start` stashes
///    the previous-stage handoff address in.  The board adapter
///    interprets it; the executor forwards it unchanged.
///
/// Every capability maps to one match arm.  Device IDs are `u8`
/// constants from `.rodata`, so LLVM folds the inner `match id` inside
/// each adapter method to a single arm and inlines it.  Net codegen
/// shape is identical to direct inlined calls.
pub fn run_stage<B: Board>(mut board: B, plan: &'static StagePlan, _handoff_ptr: usize) -> ! {
    let mut inited = DeviceMask::from_slice(plan.persistent_inited);

    for op in plan.caps {
        match *op {
            // ----- No-device capabilities ------------------------------------
            CapOp::MemoryInit => board.memory_init(),
            CapOp::LateDriverInit => board.late_driver_init_complete(0),
            CapOp::SigVerify => board.sig_verify(),
            CapOp::FdtPrepare => board.fdt_prepare(),
            CapOp::PayloadLoad => board.payload_load(),
            CapOp::StageLoad { next_stage } => board.stage_load(next_stage),
            CapOp::AcpiPrepare => board.acpi_prepare(),
            CapOp::SmBiosPrepare => board.smbios_prepare(),
            CapOp::ReturnToFel => board.return_to_fel(),

            // ----- Single-device lifecycle capabilities ----------------------
            CapOp::ClockInit(id) | CapOp::DramInit(id) => {
                // Persistent hardware-level init — skip if a previous
                // stage already did it.  The executor's `inited`
                // bitset handles that uniformly.
                if inited.contains(id) {
                    continue;
                }
                if board.init_device(id).is_err() {
                    board.halt();
                }
                inited.set(id);
            }

            CapOp::ConsoleInit(id) => {
                if board.init_device(id).is_err() {
                    board.halt();
                }
                // SAFETY: `StagePlan` guarantees `id` provides Console;
                // we just constructed it via `init_device`; the board
                // adapter holds it for the stage's lifetime.
                unsafe {
                    board.install_logger(id);
                }
                inited.set(id);
            }

            // ----- Multi-device capabilities ---------------------------------
            CapOp::ChipsetInit { nb, sb } => {
                if board.init_device(nb).is_err() {
                    board.halt();
                }
                if board.init_device(sb).is_err() {
                    board.halt();
                }
                if board.chipset_init(nb, sb).is_err() {
                    board.halt();
                }
                inited.set(nb);
                inited.set(sb);
            }

            CapOp::MpInit {
                cpu_model,
                num_cpus,
                smm,
            } => {
                // MP initialization is handled by board-specific codegen
                // which calls fstart_mp::mp_init().  The CapOp carries
                // the config; the executor passes through.
                let _ = (cpu_model, num_cpus, smm);
            }

            CapOp::PciInit(id) => {
                if board.init_device(id).is_err() {
                    board.halt();
                }
                if board.pci_init(id).is_err() {
                    board.halt();
                }
                inited.set(id);
            }

            CapOp::AcpiLoad(id) => {
                if board.init_device(id).is_err() {
                    board.halt();
                }
                if board.acpi_load(id).is_err() {
                    board.halt();
                }
                inited.set(id);
            }

            CapOp::MemoryDetect(id) => {
                if board.init_device(id).is_err() {
                    board.halt();
                }
                if board.memory_detect(id).is_err() {
                    board.halt();
                }
                inited.set(id);
            }

            // ----- Batch driver init ----------------------------------------
            CapOp::DriverInit => {
                let mut gated = DeviceMask::new();
                for (id, _) in plan.boot_media_gated {
                    gated.set(*id);
                }
                board.init_all_devices(&inited, &gated);
                for id in plan.all_devices {
                    inited.set(*id);
                }
            }

            // ----- Boot media -----------------------------------------------
            CapOp::BootMediaStatic {
                device,
                offset,
                size,
            } => {
                if let Some(id) = device {
                    if board.init_device(id).is_err() {
                        board.halt();
                    }
                    inited.set(id);
                }
                board.boot_media_static(device, offset, size);
            }

            CapOp::BootMediaAuto { candidates } => {
                let Some(id) = board.boot_media_select(candidates) else {
                    board.halt();
                };
                if board.init_device(id).is_err() {
                    board.halt();
                }
                inited.set(id);
            }

            CapOp::LoadNextStage {
                candidates,
                next_stage,
            } => {
                let Some(id) = board.boot_media_select(candidates) else {
                    board.halt();
                };
                if board.init_device(id).is_err() {
                    board.halt();
                }
                board.load_next_stage(next_stage);
            }
        }
    }

    // Reached when the plan's last cap doesn't diverge (e.g. a
    // debug-only stage that just initialises a console).  Normal
    // firmware plans have `ends_with_jump: true` and never land here.
    board.halt();
}

// ---------------------------------------------------------------------------
// Tests — exercise the executor with a MockBoard.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    //! Host-side executor tests.
    //!
    //! `run_stage` consumes `board` by value and diverges; we can't
    //! inspect the board's state after the call.  Instead the mock
    //! writes into a thread-local event log so the test can check
    //! what happened regardless of when/how `run_stage` unwound.

    extern crate std;
    use std::cell::RefCell;
    use std::vec::Vec;

    use super::*;

    /// Events recorded by [`MockBoard`] so tests can assert on the
    /// sequence of executor actions.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Event {
        // Lifecycle
        InitDevice(DeviceId),
        InitDeviceFailed(DeviceId),
        InitAllDevices {
            skip_had: Vec<DeviceId>,
            gated_had: Vec<DeviceId>,
        },
        InstallLogger(DeviceId),
        // Trampolines
        MemoryInit,
        LateDriverInitComplete(usize),
        SigVerify,
        FdtPrepare,
        PayloadLoad,
        StageLoad(String),
        AcpiPrepare,
        SmBiosPrepare,
        ChipsetInit {
            nb: DeviceId,
            sb: DeviceId,
        },
        PciInit(DeviceId),
        AcpiLoad(DeviceId),
        MemoryDetect(DeviceId),
        ReturnToFel,
        // Boot media
        BootMediaSelect(Vec<DeviceId>),
        BootMediaStatic {
            device: Option<DeviceId>,
            offset: u64,
            size: u64,
        },
        LoadNextStage(String),
        // Terminal
        Halt,
        JumpTo(u64),
    }

    use std::string::String;

    std::thread_local! {
        static EVENTS: RefCell<Vec<Event>> = const { RefCell::new(Vec::new()) };
        static FAIL_ON: RefCell<Option<DeviceId>> = const { RefCell::new(None) };
        static BOOT_MEDIA_PICK: RefCell<Option<DeviceId>> = const { RefCell::new(None) };
    }

    fn push(e: Event) {
        EVENTS.with(|v| v.borrow_mut().push(e));
    }
    fn take_events() -> Vec<Event> {
        EVENTS.with(|v| core::mem::take(&mut *v.borrow_mut()))
    }
    fn set_fail_on(id: Option<DeviceId>) {
        FAIL_ON.with(|v| *v.borrow_mut() = id);
    }
    fn should_fail(id: DeviceId) -> bool {
        FAIL_ON.with(|v| *v.borrow() == Some(id))
    }
    fn set_boot_media_pick(id: Option<DeviceId>) {
        BOOT_MEDIA_PICK.with(|v| *v.borrow_mut() = id);
    }
    fn boot_media_pick() -> Option<DeviceId> {
        BOOT_MEDIA_PICK.with(|v| *v.borrow())
    }

    /// Panic payload that `halt()` raises so the test thread can
    /// unwind cleanly out of `run_stage`.
    struct HaltSentinel;

    /// Board adapter for tests.  Records every method call into a
    /// thread-local event log so the test can assert on the executor's
    /// observable behaviour.
    struct MockBoard;

    /// Convert a `DeviceMask` to a sorted list of DeviceIds for
    /// assertion output.
    fn mask_to_vec(m: &DeviceMask) -> Vec<DeviceId> {
        (0u8..=255).filter(|id| m.contains(*id)).collect()
    }

    impl Board for MockBoard {
        // --- Lifecycle ---
        fn init_device(&mut self, id: DeviceId) -> Result<(), DeviceError> {
            if should_fail(id) {
                push(Event::InitDeviceFailed(id));
                Err(DeviceError::InitFailed)
            } else {
                push(Event::InitDevice(id));
                Ok(())
            }
        }
        fn init_all_devices(&mut self, skip: &DeviceMask, gated: &DeviceMask) {
            push(Event::InitAllDevices {
                skip_had: mask_to_vec(skip),
                gated_had: mask_to_vec(gated),
            });
        }
        unsafe fn install_logger(&self, id: DeviceId) {
            push(Event::InstallLogger(id));
        }

        // --- Trampolines ---
        fn memory_init(&self) {
            push(Event::MemoryInit);
        }
        fn late_driver_init_complete(&self, count: usize) {
            push(Event::LateDriverInitComplete(count));
        }
        fn sig_verify(&self) {
            push(Event::SigVerify);
        }
        fn fdt_prepare(&self) {
            push(Event::FdtPrepare);
        }
        fn payload_load(&self) -> ! {
            push(Event::PayloadLoad);
            push(Event::Halt);
            std::panic::panic_any(HaltSentinel);
        }
        fn stage_load(&self, next_stage: &str) -> ! {
            push(Event::StageLoad(next_stage.into()));
            push(Event::Halt);
            std::panic::panic_any(HaltSentinel);
        }
        fn acpi_prepare(&mut self) {
            push(Event::AcpiPrepare);
        }
        fn smbios_prepare(&self) {
            push(Event::SmBiosPrepare);
        }
        fn chipset_init(&mut self, nb: DeviceId, sb: DeviceId) -> Result<(), DeviceError> {
            push(Event::ChipsetInit { nb, sb });
            Ok(())
        }
        fn pci_init(&mut self, id: DeviceId) -> Result<(), DeviceError> {
            push(Event::PciInit(id));
            Ok(())
        }
        fn acpi_load(&mut self, id: DeviceId) -> Result<(), DeviceError> {
            push(Event::AcpiLoad(id));
            Ok(())
        }
        fn memory_detect(&mut self, id: DeviceId) -> Result<(), DeviceError> {
            push(Event::MemoryDetect(id));
            Ok(())
        }
        fn return_to_fel(&self) -> ! {
            push(Event::ReturnToFel);
            push(Event::Halt);
            std::panic::panic_any(HaltSentinel);
        }

        // --- Boot media ---
        fn boot_media_select(&mut self, candidates: &[BootMediaCandidate]) -> Option<DeviceId> {
            push(Event::BootMediaSelect(
                candidates.iter().map(|c| c.device).collect(),
            ));
            boot_media_pick().or_else(|| candidates.first().map(|c| c.device))
        }
        fn boot_media_static(&mut self, device: Option<DeviceId>, offset: u64, size: u64) {
            push(Event::BootMediaStatic {
                device,
                offset,
                size,
            });
        }
        fn load_next_stage(&mut self, next_stage: &str) -> ! {
            push(Event::LoadNextStage(next_stage.into()));
            push(Event::Halt);
            std::panic::panic_any(HaltSentinel);
        }

        // --- Platform primitives ---
        fn halt(&self) -> ! {
            push(Event::Halt);
            std::panic::panic_any(HaltSentinel);
        }
        fn jump_to(&self, entry: u64) -> ! {
            push(Event::JumpTo(entry));
            std::panic::panic_any(HaltSentinel);
        }
        fn jump_to_with_handoff(&self, entry: u64, _handoff: usize) -> ! {
            push(Event::JumpTo(entry));
            std::panic::panic_any(HaltSentinel);
        }
    }

    /// Run `plan` against a fresh `MockBoard` and return the events
    /// captured on the test thread.  Asserts that `run_stage` did
    /// not return.
    fn run(plan: &'static StagePlan) -> Vec<Event> {
        take_events();
        set_fail_on(None);
        set_boot_media_pick(None);
        let result = std::panic::catch_unwind(|| run_stage(MockBoard, plan, 0));
        assert!(result.is_err(), "run_stage must diverge via halt()");
        take_events()
    }

    // ===== lifecycle =========================================================

    #[test]
    fn memory_init_runs_and_halts() {
        static PLAN: StagePlan = StagePlan {
            stage_name: "t",
            is_first_stage: true,
            ends_with_jump: false,
            caps: &[CapOp::MemoryInit, CapOp::LateDriverInit],
            persistent_inited: &[],
            boot_media_gated: &[],
            all_devices: &[],
        };
        assert_eq!(
            run(&PLAN),
            [
                Event::MemoryInit,
                Event::LateDriverInitComplete(0),
                Event::Halt,
            ]
        );
    }

    #[test]
    fn console_init_constructs_and_installs_logger() {
        static PLAN: StagePlan = StagePlan {
            stage_name: "t",
            is_first_stage: true,
            ends_with_jump: false,
            caps: &[CapOp::ConsoleInit(7)],
            persistent_inited: &[],
            boot_media_gated: &[],
            all_devices: &[],
        };
        assert_eq!(
            run(&PLAN),
            [Event::InitDevice(7), Event::InstallLogger(7), Event::Halt,]
        );
    }

    #[test]
    fn clock_init_skipped_if_previous_stage_inited() {
        static PLAN: StagePlan = StagePlan {
            stage_name: "t",
            is_first_stage: false,
            ends_with_jump: false,
            caps: &[CapOp::ClockInit(3)],
            persistent_inited: &[3],
            boot_media_gated: &[],
            all_devices: &[],
        };
        assert_eq!(run(&PLAN), [Event::Halt]);
    }

    #[test]
    fn clock_init_runs_if_not_previously_inited() {
        static PLAN: StagePlan = StagePlan {
            stage_name: "t",
            is_first_stage: true,
            ends_with_jump: false,
            caps: &[CapOp::ClockInit(3)],
            persistent_inited: &[],
            boot_media_gated: &[],
            all_devices: &[],
        };
        assert_eq!(run(&PLAN), [Event::InitDevice(3), Event::Halt]);
    }

    #[test]
    fn init_device_failure_halts_before_console_install() {
        static PLAN: StagePlan = StagePlan {
            stage_name: "t",
            is_first_stage: true,
            ends_with_jump: false,
            caps: &[CapOp::ConsoleInit(9)],
            persistent_inited: &[],
            boot_media_gated: &[],
            all_devices: &[],
        };
        take_events();
        set_fail_on(Some(9));
        set_boot_media_pick(None);
        let result = std::panic::catch_unwind(|| run_stage(MockBoard, &PLAN, 0));
        assert!(result.is_err(), "run_stage must halt on failure");
        assert_eq!(take_events(), [Event::InitDeviceFailed(9), Event::Halt]);
    }

    // ===== driver init =======================================================

    #[test]
    fn driver_init_passes_inited_and_gated_masks() {
        static PLAN: StagePlan = StagePlan {
            stage_name: "t",
            is_first_stage: true,
            ends_with_jump: false,
            caps: &[CapOp::ConsoleInit(0), CapOp::DriverInit],
            persistent_inited: &[],
            boot_media_gated: &[(2, &[])],
            all_devices: &[0, 1, 2],
        };
        let events = run(&PLAN);
        let driver = events
            .iter()
            .find(|e| matches!(e, Event::InitAllDevices { .. }))
            .expect("expected InitAllDevices");
        match driver {
            Event::InitAllDevices {
                skip_had,
                gated_had,
            } => {
                // Console already init'd device 0.
                assert_eq!(skip_had, &[0u8], "skip mask should include id 0");
                assert_eq!(gated_had, &[2u8], "gated mask should include id 2");
            }
            _ => unreachable!(),
        }
    }

    // ===== trampolines =======================================================

    #[test]
    fn sig_verify_fdt_prepare_trampolines_called() {
        static PLAN: StagePlan = StagePlan {
            stage_name: "t",
            is_first_stage: true,
            ends_with_jump: false,
            caps: &[CapOp::SigVerify, CapOp::FdtPrepare],
            persistent_inited: &[],
            boot_media_gated: &[],
            all_devices: &[],
        };
        assert_eq!(
            run(&PLAN),
            [Event::SigVerify, Event::FdtPrepare, Event::Halt]
        );
    }

    #[test]
    fn payload_load_diverges() {
        static PLAN: StagePlan = StagePlan {
            stage_name: "t",
            is_first_stage: true,
            ends_with_jump: true,
            caps: &[CapOp::PayloadLoad, CapOp::MemoryInit],
            persistent_inited: &[],
            boot_media_gated: &[],
            all_devices: &[],
        };
        // MemoryInit after PayloadLoad must not be reached — PayloadLoad diverges.
        let events = run(&PLAN);
        assert!(events.contains(&Event::PayloadLoad));
        assert!(!events.contains(&Event::MemoryInit));
    }

    #[test]
    fn stage_load_passes_name() {
        static PLAN: StagePlan = StagePlan {
            stage_name: "t",
            is_first_stage: true,
            ends_with_jump: true,
            caps: &[CapOp::StageLoad { next_stage: "main" }],
            persistent_inited: &[],
            boot_media_gated: &[],
            all_devices: &[],
        };
        let events = run(&PLAN);
        assert!(events.contains(&Event::StageLoad("main".into())));
    }

    #[test]
    fn chipset_init_inits_both_and_runs_trampoline() {
        static PLAN: StagePlan = StagePlan {
            stage_name: "t",
            is_first_stage: true,
            ends_with_jump: false,
            caps: &[CapOp::ChipsetInit { nb: 5, sb: 6 }],
            persistent_inited: &[],
            boot_media_gated: &[],
            all_devices: &[],
        };
        assert_eq!(
            run(&PLAN),
            [
                Event::InitDevice(5),
                Event::InitDevice(6),
                Event::ChipsetInit { nb: 5, sb: 6 },
                Event::Halt,
            ]
        );
    }

    #[test]
    fn pci_init_inits_then_enumerates() {
        static PLAN: StagePlan = StagePlan {
            stage_name: "t",
            is_first_stage: true,
            ends_with_jump: false,
            caps: &[CapOp::PciInit(4)],
            persistent_inited: &[],
            boot_media_gated: &[],
            all_devices: &[],
        };
        assert_eq!(
            run(&PLAN),
            [Event::InitDevice(4), Event::PciInit(4), Event::Halt,]
        );
    }

    // ===== boot media ========================================================

    #[test]
    fn boot_media_state_from_static_none_is_mmio() {
        assert_eq!(
            BootMediaState::from_static(None, 0x2000_0000, 0x0200_0000),
            BootMediaState::Mmio {
                base: 0x2000_0000,
                size: 0x0200_0000,
            }
        );
    }

    #[test]
    fn boot_media_state_from_static_some_is_block() {
        assert_eq!(
            BootMediaState::from_static(Some(7), 0x8000, 0x40_0000),
            BootMediaState::Block {
                device_id: 7,
                offset: 0x8000,
                size: 0x40_0000,
            }
        );
    }

    #[test]
    fn boot_media_static_none_is_memory_mapped() {
        static PLAN: StagePlan = StagePlan {
            stage_name: "t",
            is_first_stage: true,
            ends_with_jump: false,
            caps: &[CapOp::BootMediaStatic {
                device: None,
                offset: 0x2000_0000,
                size: 0x0200_0000,
            }],
            persistent_inited: &[],
            boot_media_gated: &[],
            all_devices: &[],
        };
        assert_eq!(
            run(&PLAN),
            [
                Event::BootMediaStatic {
                    device: None,
                    offset: 0x2000_0000,
                    size: 0x0200_0000,
                },
                Event::Halt,
            ]
        );
    }

    #[test]
    fn boot_media_static_some_inits_device_first() {
        static PLAN: StagePlan = StagePlan {
            stage_name: "t",
            is_first_stage: true,
            ends_with_jump: false,
            caps: &[CapOp::BootMediaStatic {
                device: Some(4),
                offset: 0x2000,
                size: 0x40_0000,
            }],
            persistent_inited: &[],
            boot_media_gated: &[],
            all_devices: &[],
        };
        assert_eq!(
            run(&PLAN),
            [
                Event::InitDevice(4),
                Event::BootMediaStatic {
                    device: Some(4),
                    offset: 0x2000,
                    size: 0x40_0000,
                },
                Event::Halt,
            ]
        );
    }

    #[test]
    fn boot_media_auto_picks_candidate_and_inits_it() {
        static CANDIDATES: &[BootMediaCandidate] = &[
            BootMediaCandidate {
                device: 3,
                offset: 0,
                size: 0,
                media_ids: &[],
            },
            BootMediaCandidate {
                device: 4,
                offset: 0,
                size: 0,
                media_ids: &[],
            },
        ];
        static PLAN: StagePlan = StagePlan {
            stage_name: "t",
            is_first_stage: true,
            ends_with_jump: false,
            caps: &[CapOp::BootMediaAuto {
                candidates: CANDIDATES,
            }],
            persistent_inited: &[],
            boot_media_gated: &[],
            all_devices: &[],
        };
        // Force selection of the second candidate.
        take_events();
        set_fail_on(None);
        set_boot_media_pick(Some(4));
        let result = std::panic::catch_unwind(|| run_stage(MockBoard, &PLAN, 0));
        assert!(result.is_err());
        let events = take_events();
        assert!(events
            .iter()
            .any(|e| matches!(e, Event::BootMediaSelect(ids) if ids == &[3u8, 4])));
        assert!(events.contains(&Event::InitDevice(4)));
        assert!(events.contains(&Event::Halt));
    }

    #[test]
    fn boot_media_auto_halts_if_no_candidate_matches() {
        static CANDIDATES: &[BootMediaCandidate] = &[BootMediaCandidate {
            device: 3,
            offset: 0,
            size: 0,
            media_ids: &[],
        }];
        static PLAN: StagePlan = StagePlan {
            stage_name: "t",
            is_first_stage: true,
            ends_with_jump: false,
            caps: &[CapOp::BootMediaAuto {
                candidates: CANDIDATES,
            }],
            persistent_inited: &[],
            boot_media_gated: &[],
            all_devices: &[],
        };
        // Force select() to return None by also returning None from
        // the first-candidate fallback: MockBoard returns None only if
        // both the explicit pick is None AND the candidate list is empty.
        // Use an explicit impl tweak below via the shared mock but a
        // synthetic mock, simpler: set pick to some impossible id and
        // explicitly run through a variant mock.  For this minimal
        // path, use a mock that always returns None.
        struct NoMatchBoard;
        impl Board for NoMatchBoard {
            fn init_device(&mut self, id: DeviceId) -> Result<(), DeviceError> {
                MockBoard.init_device(id)
            }
            fn init_all_devices(&mut self, s: &DeviceMask, g: &DeviceMask) {
                MockBoard.init_all_devices(s, g)
            }
            unsafe fn install_logger(&self, id: DeviceId) {
                MockBoard.install_logger(id)
            }
            fn memory_init(&self) {
                MockBoard.memory_init()
            }
            fn late_driver_init_complete(&self, c: usize) {
                MockBoard.late_driver_init_complete(c)
            }
            fn sig_verify(&self) {
                MockBoard.sig_verify()
            }
            fn fdt_prepare(&self) {
                MockBoard.fdt_prepare()
            }
            fn payload_load(&self) -> ! {
                MockBoard.payload_load()
            }
            fn stage_load(&self, n: &str) -> ! {
                MockBoard.stage_load(n)
            }
            fn acpi_prepare(&mut self) {
                MockBoard.acpi_prepare()
            }
            fn smbios_prepare(&self) {
                MockBoard.smbios_prepare()
            }
            fn chipset_init(&mut self, nb: DeviceId, sb: DeviceId) -> Result<(), DeviceError> {
                MockBoard.chipset_init(nb, sb)
            }
            fn pci_init(&mut self, id: DeviceId) -> Result<(), DeviceError> {
                MockBoard.pci_init(id)
            }
            fn acpi_load(&mut self, id: DeviceId) -> Result<(), DeviceError> {
                MockBoard.acpi_load(id)
            }
            fn memory_detect(&mut self, id: DeviceId) -> Result<(), DeviceError> {
                MockBoard.memory_detect(id)
            }
            fn return_to_fel(&self) -> ! {
                MockBoard.return_to_fel()
            }
            fn boot_media_select(&mut self, candidates: &[BootMediaCandidate]) -> Option<DeviceId> {
                push(Event::BootMediaSelect(
                    candidates.iter().map(|c| c.device).collect(),
                ));
                None
            }
            fn boot_media_static(&mut self, d: Option<DeviceId>, o: u64, s: u64) {
                MockBoard.boot_media_static(d, o, s)
            }
            fn load_next_stage(&mut self, n: &str) -> ! {
                MockBoard.load_next_stage(n)
            }
            fn halt(&self) -> ! {
                MockBoard.halt()
            }
            fn jump_to(&self, e: u64) -> ! {
                MockBoard.jump_to(e)
            }
            fn jump_to_with_handoff(&self, e: u64, h: usize) -> ! {
                MockBoard.jump_to_with_handoff(e, h)
            }
        }

        take_events();
        let result = std::panic::catch_unwind(|| run_stage(NoMatchBoard, &PLAN, 0));
        assert!(result.is_err());
        let events = take_events();
        assert_eq!(events, [Event::BootMediaSelect(std::vec![3]), Event::Halt,]);
    }

    #[test]
    fn load_next_stage_selects_then_jumps() {
        static CANDIDATES: &[BootMediaCandidate] = &[BootMediaCandidate {
            device: 1,
            offset: 0,
            size: 0,
            media_ids: &[],
        }];
        static PLAN: StagePlan = StagePlan {
            stage_name: "t",
            is_first_stage: true,
            ends_with_jump: true,
            caps: &[CapOp::LoadNextStage {
                candidates: CANDIDATES,
                next_stage: "main",
            }],
            persistent_inited: &[],
            boot_media_gated: &[],
            all_devices: &[],
        };
        let events = run(&PLAN);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::BootMediaSelect(_))),
            "expected select: {events:?}"
        );
        assert!(events.contains(&Event::InitDevice(1)));
        assert!(events.contains(&Event::LoadNextStage("main".into())));
    }
}
