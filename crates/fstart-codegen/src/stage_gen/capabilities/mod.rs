//! Small helpers shared between `plan_gen` and `board_gen` for per-capability
//! codegen.
//!
//! The old file hosted one `generate_*` function per capability, emitting
//! fragments that `stage_gen::generate_fstart_main` stitched together.
//! That emission layer is gone — `board_gen` now emits all capability
//! trampolines as methods on `impl Board for _BoardDevices`, and
//! `plan_gen` emits the `StagePlan` literal.  What remains here is the
//! shared data lookups and the `acpi` / `smbios` submodules whose
//! per-variant struct emission is still reused by `board_gen`.

pub(super) mod acpi;
mod smbios;

use fstart_device_registry::DriverInstance;
use fstart_types::memory::RegionKind;
use fstart_types::{BoardConfig, DeviceConfig};

pub(super) use smbios::generate_smbios_prepare;

// ---------------------------------------------------------------------------
// DRAM region / eGON SRAM lookups
// ---------------------------------------------------------------------------

/// Select the "main" DRAM region for a board.
///
/// Prefers a region named "dram"; falls back to the largest `RegionKind::Ram`.
///
/// Exposed at `pub(super)` so `board_gen` (sibling module) can use the
/// same selection rule when populating the `_BoardDevices` DRAM fields
/// that back the [`Board::fdt_prepare`] trampoline.
pub(super) fn find_dram_region(config: &BoardConfig) -> Option<(u64, u64)> {
    if let Some(r) = config
        .memory
        .regions
        .iter()
        .find(|r| r.kind == RegionKind::Ram && r.name.as_str().contains("dram"))
    {
        return Some((r.base, r.size));
    }
    config
        .memory
        .regions
        .iter()
        .filter(|r| r.kind == RegionKind::Ram)
        .max_by_key(|r| r.size)
        .map(|r| (r.base, r.size))
}

/// Resolve the eGON header SRAM base for a multi-stage board.
///
/// The Allwinner BROM loads the bootblock into SRAM and writes a few
/// boot-context fields (boot_media, next_stage_offset / size) at fixed
/// offsets relative to the bootblock's load address.  Later stages need
/// that base to read the context at runtime.
///
/// Returns 0 for monolithic / empty stage layouts.
///
/// Exposed at `pub(in crate::stage_gen)` so `board_gen`'s
/// `boot_media_select` / `load_next_stage` trampolines can populate the
/// `_egon_sram_base` field on `_BoardDevices`.
pub(in crate::stage_gen) fn egon_sram_base(config: &BoardConfig) -> u64 {
    match &config.stages {
        fstart_types::StageLayout::MultiStage(stages) => {
            stages.first().map(|s| s.load_addr).unwrap_or(0)
        }
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// Boot media value inference
// ---------------------------------------------------------------------------

/// Determine the SoC boot-source register values that correspond to a device.
///
/// Delegates to [`DriverInstance::boot_media_values()`] on the device's
/// driver instance.  The mapping constants live in
/// `fstart-device-registry` (co-located with driver metadata), not here.
///
/// Returns an empty `Vec` for drivers that have no boot-source mapping.
pub(crate) fn boot_media_values_for_device(
    dev_name: &str,
    devices: &[DeviceConfig],
    instances: &[DriverInstance],
) -> Vec<u8> {
    let Some(idx) = devices.iter().position(|d| d.name.as_str() == dev_name) else {
        // Device not in the board's device list — return empty rather
        // than panicking, so boards with multi-device BootMediaAuto
        // degrade gracefully when a candidate is absent.
        return Vec::new();
    };
    instances[idx].boot_media_values()
}
