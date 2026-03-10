//! Load and parse board.ron files.
//!
//! Performs two-phase parsing:
//! 1. Deserialize into [`RonBoardConfig`] — an internal type where
//!    each device's `driver` field is a [`DriverInstance`] enum variant
//!    that carries the typed, driver-specific config.  Hierarchy is
//!    expressed via nested `children` — no parent string references.
//! 2. Flatten the nested tree into parallel arrays in [`ParsedBoard`]:
//!    a [`BoardConfig`] (metadata), [`DriverInstance`] configs, and
//!    [`DeviceNode`] index table — all in topological (pre-order) order.
//!
//! This keeps `fstart-types` independent of driver crate details while
//! giving codegen compile-time-validated, typed configs.

use std::path::Path;

use heapless::String as HString;
use serde::Deserialize;

use fstart_device_registry::DriverInstance;
use fstart_types::{
    BoardConfig, BuildMode, DeviceConfig, DeviceId, DeviceNode, MemoryMap, PayloadConfig, Platform,
    SecurityConfig, SocImageFormat, StageLayout,
};

/// A fully-parsed board configuration.
///
/// Combines the metadata in [`BoardConfig`] with the validated, typed
/// driver configurations from [`DriverInstance`].
///
/// All three parallel arrays (`config.devices`, `driver_instances`,
/// `device_tree`) share the same indices — `device_tree[i]` describes
/// the hierarchy position of `config.devices[i]` / `driver_instances[i]`.
///
/// Devices are in topological (pre-order DFS) order: parents always
/// appear before their children.
pub struct ParsedBoard {
    /// Board metadata (name, platform, memory, stages, security, etc.).
    pub config: BoardConfig,
    /// Typed driver configs, one per device, parallel to `config.devices`.
    pub driver_instances: Vec<DriverInstance>,
    /// Flat index-based device tree, parallel to `config.devices`.
    pub device_tree: Vec<DeviceNode>,
}

// -----------------------------------------------------------------------
// Internal deserialization types (match the RON schema)
// -----------------------------------------------------------------------

/// Board config as it appears in the RON file.
///
/// Identical to [`BoardConfig`] except `devices` carries the full
/// [`DriverInstance`] and supports nested `children`.
#[derive(Deserialize)]
struct RonBoardConfig {
    name: HString<64>,
    platform: Platform,
    memory: MemoryMap,
    devices: Vec<RonDevice>,
    stages: StageLayout,
    security: SecurityConfig,
    mode: BuildMode,
    payload: Option<PayloadConfig>,
    #[serde(default)]
    soc_image_format: SocImageFormat,
    #[serde(default)]
    acpi: Option<fstart_types::acpi::AcpiConfig>,
}

/// A single device entry in the RON file.
///
/// Hierarchy is expressed structurally: a bus controller lists its
/// children inline.  No `parent` string references needed — the
/// tree structure IS the hierarchy.
///
/// ```ron
/// (
///     name: "i2c0",
///     driver: DesignwareI2c(( base_addr: 0x10030000, ... )),
///     services: ["I2cBus"],
///     children: [
///         ( name: "tpm0", driver: Slb9670(( addr: 0x50 )), services: ["Tpm"] ),
///     ],
/// )
/// ```
#[derive(Deserialize)]
struct RonDevice {
    name: HString<32>,
    services: heapless::Vec<HString<32>, 8>,
    /// Typed enum variant: `Ns16550(( base_addr: …, … ))`, `Pl011(( … ))`, etc.
    driver: DriverInstance,
    /// Child devices attached to this bus controller.
    /// Empty for leaf devices (default when omitted in RON).
    #[serde(default)]
    children: Vec<RonDevice>,
}

// -----------------------------------------------------------------------
// Public API
// -----------------------------------------------------------------------

/// Load and fully validate a board config from a RON file.
///
/// Returns a [`ParsedBoard`] with metadata in `config`, typed driver
/// configs in `driver_instances`, and the flat device tree in
/// `device_tree`.  Used by `fstart-stage/build.rs` and the stage
/// generator.
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
// Conversion — flatten nested tree into parallel arrays
// -----------------------------------------------------------------------

/// Convert the RON-deserialized board config into a [`ParsedBoard`].
///
/// Performs a pre-order DFS of the nested device tree, producing three
/// parallel arrays where parents always precede children.
fn convert(ron: RonBoardConfig) -> ParsedBoard {
    let mut devices = heapless::Vec::new();
    let mut driver_instances = Vec::new();
    let mut device_tree = Vec::new();

    // Flatten each top-level device (and its children) via DFS.
    for rd in ron.devices {
        flatten_device(
            rd,
            None,
            0,
            &mut devices,
            &mut driver_instances,
            &mut device_tree,
        );
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
        soc_image_format: ron.soc_image_format,
        acpi: ron.acpi,
    };

    ParsedBoard {
        config,
        driver_instances,
        device_tree,
    }
}

/// Recursively flatten a device and its children in pre-order DFS.
///
/// The parent is appended first, then each child is flattened with
/// `parent_idx` pointing back to the parent.
fn flatten_device(
    rd: RonDevice,
    parent_idx: Option<DeviceId>,
    depth: u8,
    devices: &mut heapless::Vec<DeviceConfig, 32>,
    driver_instances: &mut Vec<DriverInstance>,
    device_tree: &mut Vec<DeviceNode>,
) {
    let my_idx = devices.len() as DeviceId;

    let driver_name = rd.driver.driver_name();
    let parent_name = parent_idx.map(|idx| devices[idx as usize].name.clone());

    let _ = devices.push(DeviceConfig {
        name: rd.name,
        driver: HString::try_from(driver_name)
            .unwrap_or_else(|_| panic!("driver name '{driver_name}' exceeds HString<32> capacity")),
        services: rd.services,
        parent: parent_name,
    });
    driver_instances.push(rd.driver);
    device_tree.push(DeviceNode {
        parent: parent_idx,
        depth,
    });

    // Recurse into children — they get `my_idx` as their parent.
    for child in rd.children {
        flatten_device(
            child,
            Some(my_idx),
            depth + 1,
            devices,
            driver_instances,
            device_tree,
        );
    }
}
