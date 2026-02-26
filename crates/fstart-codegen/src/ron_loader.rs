//! Load and parse board.ron files.
//!
//! Performs two-phase parsing:
//! 1. Deserialize into [`RonBoardConfig`] — an internal type where
//!    each device's `driver` field is a [`DriverInstance`] enum variant
//!    that carries the typed, driver-specific config.
//! 2. Convert to a [`ParsedBoard`] containing a [`BoardConfig`]
//!    (metadata only) alongside the validated [`DriverInstance`] values.
//!
//! This keeps `fstart-types` independent of driver crate details while
//! giving codegen compile-time-validated, typed configs.

use std::path::Path;

use heapless::String as HString;
use serde::Deserialize;

use fstart_drivers::DriverInstance;
use fstart_types::{
    BoardConfig, BuildMode, DeviceConfig, MemoryMap, PayloadConfig, SecurityConfig, StageLayout,
};

/// A fully-parsed board configuration.
///
/// Combines the metadata in [`BoardConfig`] with the validated, typed
/// driver configurations from [`DriverInstance`].
///
/// `driver_instances[i]` is the config for `config.devices[i]`.
pub struct ParsedBoard {
    /// Board metadata (name, platform, memory, stages, security, etc.).
    pub config: BoardConfig,
    /// Typed driver configs, one per device, in the same order as
    /// `config.devices`.
    pub driver_instances: Vec<DriverInstance>,
}

// -----------------------------------------------------------------------
// Internal deserialization types (match the new RON schema)
// -----------------------------------------------------------------------

/// Board config as it appears in the RON file.
///
/// Identical to [`BoardConfig`] except `devices` carries the full
/// [`DriverInstance`] rather than a plain driver-name string.
#[derive(Deserialize)]
struct RonBoardConfig {
    name: HString<64>,
    platform: HString<32>,
    memory: MemoryMap,
    devices: Vec<RonDevice>,
    stages: StageLayout,
    security: SecurityConfig,
    mode: BuildMode,
    payload: Option<PayloadConfig>,
}

/// A single device entry in the RON file.
#[derive(Deserialize)]
struct RonDevice {
    name: HString<32>,
    services: heapless::Vec<HString<32>, 8>,
    /// Typed enum variant: `Ns16550(base_addr: …, …)`, `Pl011(…)`, etc.
    driver: DriverInstance,
    #[serde(default)]
    parent: Option<HString<32>>,
}

// -----------------------------------------------------------------------
// Public API
// -----------------------------------------------------------------------

/// Load and fully validate a board config from a RON file.
///
/// Returns a [`ParsedBoard`] with metadata in `config` and typed driver
/// configs in `driver_instances`.  Used by `fstart-stage/build.rs` and
/// the stage generator.
pub fn load_parsed_board(path: &Path) -> Result<ParsedBoard, String> {
    let contents = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    let ron_cfg: RonBoardConfig =
        ron::from_str(&contents).map_err(|e| format!("failed to parse {}: {e}", path.display()))?;
    Ok(convert(ron_cfg))
}

/// Load only the [`BoardConfig`] metadata (no driver instance data).
///
/// Convenience wrapper for callers that don't need the typed configs
/// (e.g., xtask feature derivation).
pub fn load_board_config(path: &Path) -> Result<BoardConfig, String> {
    let parsed = load_parsed_board(path)?;
    Ok(parsed.config)
}

// -----------------------------------------------------------------------
// Conversion
// -----------------------------------------------------------------------

/// Convert the RON-deserialized board config into a [`ParsedBoard`].
fn convert(ron: RonBoardConfig) -> ParsedBoard {
    let mut devices = heapless::Vec::new();
    let mut driver_instances = Vec::with_capacity(ron.devices.len());

    for rd in ron.devices {
        let driver_name = rd.driver.driver_name();
        let _ = devices.push(DeviceConfig {
            name: rd.name,
            driver: HString::try_from(driver_name).unwrap_or_else(|_| {
                panic!("driver name '{driver_name}' exceeds HString<32> capacity")
            }),
            services: rd.services,
            parent: rd.parent,
        });
        driver_instances.push(rd.driver);
    }

    let config = BoardConfig {
        name: ron.name,
        platform: ron.platform,
        memory: ron.memory,
        devices,
        stages: ron.stages,
        security: ron.security,
        mode: ron.mode,
        payload: ron.payload,
    };

    ParsedBoard {
        config,
        driver_instances,
    }
}
