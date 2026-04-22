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

/// Determine the eGON `boot_media` values that correspond to a device.
///
/// Maps a device name to the BROM boot_media constants based on the
/// device's driver type and configuration.  Used by `plan_gen` and
/// `board_gen` to emit match arms for runtime boot-device
/// auto-detection.
pub(crate) fn boot_media_values_for_device(
    dev_name: &str,
    devices: &[DeviceConfig],
    instances: &[DriverInstance],
) -> Vec<u8> {
    let Some(idx) = devices.iter().position(|d| d.name.as_str() == dev_name) else {
        panic!(
            "boot_media_values_for_device: device '{}' not found in board devices list",
            dev_name
        );
    };
    let inst = &instances[idx];
    let driver_name = inst.meta().name;

    match driver_name {
        "sunxi-mmc" => {
            // All sunxi MMC controllers share the same eGON boot_media
            // constants. Extract mmc_index via the SunxiMmcConfig helper.
            if let DriverInstance::SunxiMmc(cfg) = inst {
                match cfg.mmc_index() {
                    0 => vec![0x00, 0x10], // BOOT_MEDIA_MMC0, BOOT_MEDIA_MMC0_HIGH
                    2 => vec![0x02, 0x12], // BOOT_MEDIA_MMC2, BOOT_MEDIA_MMC2_HIGH
                    other => panic!(
                        "boot_media_values_for_device: unsupported mmc_index {} for device '{}'",
                        other, dev_name
                    ),
                }
            } else {
                unreachable!("driver name is sunxi-mmc but instance is not SunxiMmc")
            }
        }
        "sunxi-spi" => {
            vec![0x03] // BOOT_MEDIA_SPI
        }
        other => panic!(
            "boot_media_values_for_device: driver '{}' on device '{}' has no known boot_media mapping",
            other, dev_name
        ),
    }
}
