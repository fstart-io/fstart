//! The compiled stage plan consumed by [`run_stage`](super::run_stage).
//!
//! Everything in this module is `Copy` and lives in `.rodata` on the
//! firmware. Codegen emits a single
//!
//! ```ignore
//! #[no_mangle]
//! pub static PLAN: StagePlan = StagePlan { /* ... */ };
//! ```
//!
//! and the handwritten executor reads it at stage entry.  No
//! construction, no heap.
//!
//! See `.opencode/plans/stage-runtime-codegen-split.md` for the design
//! rationale.

use fstart_types::DeviceId;

// ---------------------------------------------------------------------------
// CapOp — one entry per executed capability, in declared order
// ---------------------------------------------------------------------------

/// A single capability step.  The codegen resolves every device name to
/// a [`DeviceId`] before emission, so the executor never touches
/// strings.
///
/// This mirrors [`fstart_types::Capability`] but with:
///
/// - names resolved to `DeviceId`
/// - no `heapless::String` fields (all strings become `&'static str`)
/// - no nested configuration structs (everything is a small tagged
///   union of primitives or `&'static` references into codegen-emitted
///   tables)
///
/// Because every variant is small and `Copy`, the whole
/// [`StagePlan::caps`] slice is just `.rodata` bytes.
#[derive(Debug, Clone, Copy)]
pub enum CapOp {
    /// `ClockInit { device }` — program the clock controller.
    ClockInit(DeviceId),
    /// `ConsoleInit { device }` — bring up the early console and
    /// install the global logger.
    ConsoleInit(DeviceId),
    /// `MemoryInit` — stub memory init (QEMU-style boards).
    MemoryInit,
    /// `DramInit { device }` — real DRAM training on a controller.
    DramInit(DeviceId),
    /// `ChipsetInit { northbridge, southbridge }` — x86 chipset unlock.
    ChipsetInit { nb: DeviceId, sb: DeviceId },
    /// `MpInit` — bring up APs, per-CPU MSR config, park.
    MpInit {
        cpu_model: &'static str,
        num_cpus: u16,
        smm: bool,
    },
    /// `PciInit { device }` — enumerate and allocate a PCI root bus.
    PciInit(DeviceId),
    /// `DriverInit` — init every remaining enabled device.
    DriverInit,
    /// `LateDriverInit` — post-payload lockdown phase.
    LateDriverInit,
    /// `SigVerify` — verify the FFS manifest signature.
    SigVerify,
    /// `FdtPrepare` — patch the platform FDT for OS handoff.
    FdtPrepare,
    /// `PayloadLoad` — load the payload and jump to it.  Does not
    /// return.
    PayloadLoad,
    /// `StageLoad { next_stage }` — load the named next stage from
    /// FFS into RAM and jump to it.  Does not return.
    StageLoad { next_stage: &'static str },
    /// `AcpiPrepare` — generate ACPI tables from the board RON.
    AcpiPrepare,
    /// `SmBiosPrepare` — write SMBIOS tables from the board RON.
    SmBiosPrepare,
    /// `AcpiLoad { device }` — load ACPI tables from an external
    /// provider (e.g. QEMU fw_cfg).
    AcpiLoad(DeviceId),
    /// `MemoryDetect { device }` — read the memory map from a runtime
    /// source.
    MemoryDetect(DeviceId),
    /// `BootMedia` with a single static candidate (memory-mapped flash
    /// or a single block device).  The executor just records the
    /// selection; reads go through [`Board::boot_media_read`].
    ///
    /// [`Board::boot_media_read`]: super::Board::boot_media_read
    BootMediaStatic {
        /// `None` means the memory-mapped flash path; `Some(id)` picks
        /// that block device.
        device: Option<DeviceId>,
        offset: u64,
        size: u64,
    },
    /// `BootMedia(AutoDevice)` — pick a candidate at runtime based on
    /// the hardware boot-source register.
    BootMediaAuto {
        candidates: &'static [BootMediaCandidate],
    },
    /// `LoadNextStage { devices, next_stage }` — like `StageLoad` but
    /// the source device is picked at runtime.
    LoadNextStage {
        candidates: &'static [BootMediaCandidate],
        next_stage: &'static str,
    },
    /// `ReturnToFel` — armv7 sunxi-only; return to BROM USB recovery.
    ReturnToFel,
}

/// One entry in a `BootMediaAuto` / `LoadNextStage` candidate table.
///
/// Lives in `.rodata`.  The executor asks the board, at runtime, for
/// the current hardware boot-source byte; whichever candidate's
/// [`media_ids`](Self::media_ids) contains that byte wins.
#[derive(Debug, Clone, Copy)]
pub struct BootMediaCandidate {
    /// Device to use as the boot medium.
    pub device: DeviceId,
    /// Offset into the device where the FFS image starts.
    pub offset: u64,
    /// Size of the FFS image on the device.
    pub size: u64,
    /// Boot-source register values that select this candidate.
    ///
    /// Example: on Allwinner eGON the SRAM A1 boot-media byte is
    /// `0x00` for MMC0, `0x10` for MMC0-HIGH, `0x03` for SPI, etc.
    pub media_ids: &'static [u8],
}

// ---------------------------------------------------------------------------
// StagePlan — the root object codegen emits per stage
// ---------------------------------------------------------------------------

/// Everything [`run_stage`](super::run_stage) needs to execute one stage.
///
/// All fields are `&'static` slices or `Copy` scalars; the whole object
/// lives in `.rodata`.
#[derive(Debug, Clone, Copy)]
pub struct StagePlan {
    /// Stage name for diagnostics (e.g. `"bootblock"`).
    pub stage_name: &'static str,
    /// `true` for monolithic builds or the first stage of a multi-stage
    /// build.  Used to decide whether to deserialize a handoff from a
    /// previous stage.
    pub is_first_stage: bool,
    /// `true` when the stage's last capability hands control off
    /// (`PayloadLoad`, `StageLoad`, `LoadNextStage`, `ReturnToFel`).
    /// When set, the executor skips the "all capabilities complete"
    /// banner.
    pub ends_with_jump: bool,
    /// Capabilities in declared order.
    pub caps: &'static [CapOp],
    /// Devices that a previous stage already initialised *and* whose
    /// hardware state persists (ClockInit, DramInit).  Re-initialising
    /// them here would be redundant or catastrophic.
    pub persistent_inited: &'static [DeviceId],
    /// `(device, boot_media_ids)` pairs gating a device's
    /// `DriverInit`.  When the current boot medium does not match
    /// the device's `boot_media_ids`, the executor skips `init()`.
    pub boot_media_gated: &'static [(DeviceId, &'static [u8])],
    /// All enabled, non-structural, non-ACPI-only devices in
    /// root-first order.  `DriverInit` iterates this list.
    pub all_devices: &'static [DeviceId],
}

impl StagePlan {
    /// A minimal plan used by tests and as a safe default.
    ///
    /// Not `const` because `&'static []` array construction used to
    /// need `const_fn_trait_bound` on older toolchains; today it
    /// compiles fine as a plain `const`.
    pub const EMPTY: StagePlan = StagePlan {
        stage_name: "",
        is_first_stage: true,
        ends_with_jump: false,
        caps: &[],
        persistent_inited: &[],
        boot_media_gated: &[],
        all_devices: &[],
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizeof_capop_stays_small() {
        // Guard against accidental bloat — CapOp is in .rodata per-stage
        // and its size multiplies by the capability count.  A sudden
        // jump would usually mean somebody stuffed a large struct into
        // a variant and should use a &'static reference instead.
        //
        // Current layout is dominated by `BootMediaStatic { device:
        // Option<DeviceId>, offset: u64, size: u64 }` = 24 payload
        // bytes + a 1-byte tag + padding = 40 bytes total.  If we
        // need it smaller later, move the static descriptor behind a
        // `&'static BootMediaStatic` reference (like the auto
        // candidate table already is).
        let sz = core::mem::size_of::<CapOp>();
        assert!(
            sz <= 48,
            "CapOp is {sz} bytes; cap is 48.  Did a variant gain a heavy field?",
        );
    }

    #[test]
    fn empty_plan_is_valid() {
        let p = StagePlan::EMPTY;
        assert!(p.caps.is_empty());
        assert!(p.all_devices.is_empty());
    }
}
