//! Load Rust board metadata and legacy RON board descriptions.
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

use std::{collections::HashMap, path::Path};

use heapless::String as HString;
use serde::Deserialize;

use fstart_device_registry::{
    DriverBinding, DriverInstance, Service, ServiceSet, StructuralConfig, StructuralKind,
};
use fstart_types::acpi::AcpiExtraDevice;
use fstart_types::device::BusAddress;
use fstart_types::{
    BoardConfig, DeviceConfig, DeviceId, DeviceNode, DeviceRole, MemoryMap, PayloadConfig,
    Platform, SecurityConfig, SocImageFormat, StageLayout,
};

fn default_enabled() -> bool {
    true
}

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
    /// Effective service set per device after applying board policy.
    pub device_services: Vec<ServiceSet>,
    /// ACPI-only descriptors collected separately from runtime devices.
    pub acpi_only_devices: Vec<AcpiExtraDevice>,
}

// -----------------------------------------------------------------------
// Internal deserialization types (match the RON schema)
// -----------------------------------------------------------------------

/// Board config as it appears in the RON file.
///
/// Identical to [`BoardConfig`] except `devices` carries the full
/// [`DriverInstance`] and supports nested `children`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RonBoardConfig {
    name: HString<64>,
    platform: Platform,
    memory: MemoryMap,
    devices: Vec<RonDevice>,
    stages: StageLayout,
    security: SecurityConfig,
    payload: Option<PayloadConfig>,
    #[serde(default)]
    microcode: Option<fstart_types::board::MicrocodeConfig>,
    #[serde(default)]
    soc_image_format: SocImageFormat,
    #[serde(default)]
    full_flash_image: bool,
    #[serde(default)]
    acpi: Option<fstart_types::acpi::AcpiConfig>,
    #[serde(default)]
    smbios: Option<fstart_types::smbios::SmbiosConfig>,
    #[serde(default)]
    smm: Option<fstart_types::smm::SmmConfig>,
    #[serde(default)]
    boot_hart_id: u32,
}

#[derive(Debug, Clone, Copy, Deserialize)]
enum RonDeviceKind {
    Structural(StructuralKind),
    AcpiOnly,
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
///     children: [
///         ( name: "tpm0", driver: Slb9670(( addr: 0x50 )) ),
///     ],
/// )
/// ```
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RonDevice {
    name: HString<32>,
    /// Board policy: suppress selected services this driver can provide.
    ///
    /// Example: `disabled_services: [Console]` leaves the device present
    /// but prevents generated console/logger paths from selecting it.
    #[serde(default)]
    disabled_services: heapless::Vec<Service, 16>,
    /// Typed device kind for non-runtime topology nodes.
    ///
    /// Runtime devices use `driver`; structural nodes use
    /// `kind: Structural(...)` and must not also set `driver`.
    /// ACPI-only descriptors use `kind: AcpiOnly` with an ACPI-only
    /// driver descriptor and are collected outside the runtime device table.
    #[serde(default)]
    kind: Option<RonDeviceKind>,
    /// Runtime driver enum variant: `Ns16550(( base_addr: …, … ))`, `Pl011(( … ))`, etc.
    #[serde(default)]
    driver: Option<DriverInstance>,
    /// ACPI-only descriptor enum variant: `Ahci((...))`, `Xhci((...))`, etc.
    #[serde(default)]
    acpi: Option<AcpiExtraDevice>,
    /// Child devices attached to this bus controller.
    /// Empty for leaf devices (default when omitted in RON).
    #[serde(default)]
    children: Vec<RonDevice>,
    /// Physical attachment to the parent bus (optional).
    #[serde(default)]
    bus: Option<BusAddress>,
    /// Whether this device is enabled (default `true`).
    #[serde(default = "default_enabled")]
    enabled: bool,
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
    load_parsed_board_from_str(&contents, &path.display().to_string())
}

/// Load and fully validate a legacy board config from an in-memory RON string.
///
/// Migrated Rust board crates should use [`load_parsed_board_from_rust`] instead.
pub fn load_parsed_board_from_str(contents: &str, source: &str) -> Result<ParsedBoard, String> {
    // Enable `implicit_some` so legacy RON can
    // write `field: 42` for `Option<T>` schema fields without wrapping in
    // `Some(42)`.
    let options =
        ron::Options::default().with_default_extension(ron::extensions::Extensions::IMPLICIT_SOME);
    let mut ron_cfg: RonBoardConfig = options
        .from_str(contents)
        .map_err(|e| format!("failed to parse {source}: {e}"))?;
    normalize_ron_config(&mut ron_cfg)?;
    convert(ron_cfg)
}

/// Load and fully validate a board from native Rust metadata.
///
/// This is the build-time entry point for migrated board crates. The board crate
/// constructs typed [`BoardConfig`] and [`DriverInstance`] values directly, and
/// this helper derives the flattened topology and effective service tables from
/// those Rust values. No RON/JSON/postcard transport is involved.
pub fn load_parsed_board_from_rust(
    config: BoardConfig,
    driver_bindings: Vec<DriverBinding>,
) -> Result<ParsedBoard, String> {
    load_parsed_board_from_rust_with_acpi(config, driver_bindings, Vec::new())
}

/// Load and fully validate a board from native Rust metadata plus ACPI-only devices.
///
/// ACPI-only descriptors are side-table metadata for table generation. They are
/// not runtime devices and therefore do not participate in the flat runtime
/// topology or driver binding validation.
pub fn load_parsed_board_from_rust_with_acpi(
    mut config: BoardConfig,
    driver_bindings: Vec<DriverBinding>,
    acpi_only_devices: Vec<AcpiExtraDevice>,
) -> Result<ParsedBoard, String> {
    config
        .memory
        .normalize_derived_flash()
        .map_err(|err| err.to_string())?;

    let mut bindings_by_device = HashMap::with_capacity(driver_bindings.len());
    for binding in driver_bindings {
        let device = binding.device.to_string();
        if bindings_by_device
            .insert(device.clone(), binding.instance)
            .is_some()
        {
            return Err(format!(
                "board '{}' has duplicate driver binding for device '{}'",
                config.name, device
            ));
        }
    }

    let mut device_tree: Vec<DeviceNode> = Vec::with_capacity(config.devices.len());
    let mut driver_instances: Vec<DriverInstance> = Vec::with_capacity(config.devices.len());
    let mut device_services: Vec<ServiceSet> = Vec::with_capacity(config.devices.len());

    for (idx, device) in config.devices.iter().enumerate() {
        let parent_idx = match &device.parent {
            Some(parent_name) => Some(
                config
                    .devices
                    .iter()
                    .take(idx)
                    .position(|candidate| candidate.name == *parent_name)
                    .ok_or_else(|| {
                        format!(
                            "device '{}' references missing or later parent '{}'",
                            device.name, parent_name
                        )
                    })? as DeviceId,
            ),
            None => None,
        };
        let depth = parent_idx
            .map(|parent_idx| device_tree[parent_idx as usize].depth.saturating_add(1))
            .unwrap_or(0);
        device_tree.push(DeviceNode {
            parent: parent_idx,
            depth,
        });

        let instance = if device.role.is_runtime() {
            bindings_by_device
                .remove(device.name.as_str())
                .ok_or_else(|| {
                    format!(
                        "runtime device '{}' in board '{}' has no named driver binding",
                        device.name, config.name
                    )
                })?
        } else {
            if bindings_by_device.contains_key(device.name.as_str()) {
                return Err(format!(
                    "structural device '{}' in board '{}' must not have a runtime driver binding",
                    device.name, config.name
                ));
            }
            DriverInstance::Structural(StructuralConfig {
                kind: structural_kind_for_role(device.role)?,
            })
        };
        device_services.push(instance.provided_services());
        driver_instances.push(instance);
    }

    if !bindings_by_device.is_empty() {
        let mut names: Vec<_> = bindings_by_device.keys().cloned().collect();
        names.sort();
        return Err(format!(
            "board '{}' has driver bindings for unknown devices: {}",
            config.name,
            names.join(", ")
        ));
    }

    Ok(ParsedBoard {
        config,
        driver_instances,
        device_tree,
        device_services,
        acpi_only_devices,
    })
}

fn structural_kind_for_role(role: DeviceRole) -> Result<StructuralKind, String> {
    match role {
        DeviceRole::Runtime => Err("runtime device role is not structural".to_string()),
        DeviceRole::PciBridge => Ok(StructuralKind::PciBridge),
        DeviceRole::LpcBus => Ok(StructuralKind::LpcBus),
        DeviceRole::SmBus => Ok(StructuralKind::SmBus),
        DeviceRole::GenericBus => Ok(StructuralKind::GenericBus),
    }
}

/// Load only the [`BoardConfig`] metadata (no driver instance data).
///
/// Convenience wrapper for callers that don't need the typed configs
/// (e.g., xtask feature derivation).
pub fn load_board_config(path: &Path) -> Result<BoardConfig, String> {
    let parsed = load_parsed_board(path)?;
    Ok(parsed.config)
}

/// Load only [`BoardConfig`] metadata from an in-memory board description.
pub fn load_board_config_from_str(contents: &str, source: &str) -> Result<BoardConfig, String> {
    let parsed = load_parsed_board_from_str(contents, source)?;
    Ok(parsed.config)
}

// -----------------------------------------------------------------------
// Normalization — derive redundant board facts and expand shorthands
// -----------------------------------------------------------------------

/// Normalize deserialized RON before flattening.
///
/// Intel IFD boards describe a flash partition layout; this helper only derives
/// linker-visible ROM regions needed for those layouts.  Firmware-image boot
/// media is no longer resolved from the RON memory map — generated stages use
/// hardware `FirmwareImageProvider` services instead.
fn normalize_ron_config(ron: &mut RonBoardConfig) -> Result<(), String> {
    ron.memory
        .normalize_derived_flash()
        .map_err(|err| err.to_string())
}

// -----------------------------------------------------------------------
// Conversion — flatten nested tree into parallel arrays
// -----------------------------------------------------------------------

/// Convert the RON-deserialized board config into a [`ParsedBoard`].
///
/// Performs a pre-order DFS of the nested device tree, producing three
/// parallel arrays where parents always precede children.
fn convert(ron: RonBoardConfig) -> Result<ParsedBoard, String> {
    let mut devices = heapless::Vec::new();
    let mut driver_instances = Vec::new();
    let mut device_tree = Vec::new();
    let mut device_services = Vec::new();
    let mut acpi_only_devices: Vec<AcpiExtraDevice> = Vec::new();

    let mut state = FlattenState {
        devices: &mut devices,
        driver_instances: &mut driver_instances,
        device_tree: &mut device_tree,
        device_services: &mut device_services,
        acpi_only_devices: &mut acpi_only_devices,
    };

    // Flatten each top-level device (and its children) via DFS.
    for rd in ron.devices {
        flatten_device(rd, None, 0, &mut state)?;
    }

    let config = BoardConfig {
        name: ron.name,
        platform: ron.platform,
        memory: ron.memory,
        devices,
        stages: ron.stages,
        security: ron.security,
        payload: ron.payload,
        microcode: ron.microcode,
        soc_image_format: ron.soc_image_format,
        full_flash_image: ron.full_flash_image,
        acpi: ron.acpi,
        smbios: ron.smbios,
        smm: ron.smm,
        boot_hart_id: ron.boot_hart_id,
    };

    Ok(ParsedBoard {
        config,
        driver_instances,
        device_tree,
        device_services,
        acpi_only_devices,
    })
}

struct FlattenState<'a> {
    devices: &'a mut heapless::Vec<DeviceConfig, 32>,
    driver_instances: &'a mut Vec<DriverInstance>,
    device_tree: &'a mut Vec<DeviceNode>,
    device_services: &'a mut Vec<ServiceSet>,
    acpi_only_devices: &'a mut Vec<AcpiExtraDevice>,
}

/// Recursively flatten a device and its children in pre-order DFS.
///
/// The parent is appended first, then each child is flattened with
/// `parent_idx` pointing back to the parent.
fn flatten_device(
    rd: RonDevice,
    parent_idx: Option<DeviceId>,
    depth: u8,
    state: &mut FlattenState<'_>,
) -> Result<(), String> {
    if matches!(rd.kind, Some(RonDeviceKind::AcpiOnly)) {
        return flatten_acpi_only_device(rd, state);
    }

    let my_idx = state.devices.len() as DeviceId;

    // Structural nodes become explicit instances in the typed driver instance
    // table. DeviceConfig stays pure topology metadata.
    let (instance, role) = match (rd.driver, rd.kind, rd.acpi) {
        (_, None, Some(_)) => {
            return Err(format!(
                "ACPI-only descriptor '{}' must use 'kind: AcpiOnly'",
                rd.name
            ));
        }
        (Some(instance), None, None) => (instance, DeviceRole::Runtime),
        (None, Some(RonDeviceKind::Structural(kind)), None) => (
            DriverInstance::Structural(StructuralConfig { kind }),
            role_for_structural_kind(kind),
        ),
        (Some(_), Some(RonDeviceKind::Structural(_)), _)
        | (_, Some(RonDeviceKind::Structural(_)), Some(_)) => {
            return Err(format!(
                "device '{}' specifies both runtime/ACPI descriptor and structural 'kind'; choose one",
                rd.name
            ));
        }
        (None, None, None) => {
            return Err(format!(
                "device '{}' is missing 'driver', 'kind: Structural(...)', or 'kind: AcpiOnly'",
                rd.name
            ));
        }
        (_, Some(RonDeviceKind::AcpiOnly), _) => unreachable!("ACPI-only handled above"),
    };
    let parent_name = parent_idx.map(|idx| state.devices[idx as usize].name.clone());

    let effective_services = effective_services(&instance, &rd.disabled_services)?;
    state
        .devices
        .push(DeviceConfig {
            name: rd.name,
            parent: parent_name,
            bus: rd.bus,
            role,
            enabled: rd.enabled,
        })
        .map_err(|_| "board declares more than 32 runtime/structural devices".to_string())?;
    state.driver_instances.push(instance);
    state.device_services.push(effective_services);
    state.device_tree.push(DeviceNode {
        parent: parent_idx,
        depth,
    });

    // Recurse into children — they get `my_idx` as their parent.
    for child in rd.children {
        flatten_device(child, Some(my_idx), depth + 1, state)?;
    }

    Ok(())
}

fn role_for_structural_kind(kind: StructuralKind) -> DeviceRole {
    match kind {
        StructuralKind::PciBridge => DeviceRole::PciBridge,
        StructuralKind::LpcBus => DeviceRole::LpcBus,
        StructuralKind::SmBus => DeviceRole::SmBus,
        StructuralKind::GenericBus => DeviceRole::GenericBus,
    }
}

fn flatten_acpi_only_device(rd: RonDevice, state: &mut FlattenState<'_>) -> Result<(), String> {
    if !rd.children.is_empty() {
        return Err(format!(
            "ACPI-only descriptor '{}' cannot have child devices",
            rd.name
        ));
    }

    if rd.driver.is_some() {
        return Err(format!(
            "device '{}' uses 'kind: AcpiOnly' with a runtime driver",
            rd.name
        ));
    }
    let Some(acpi_device) = rd.acpi else {
        return Err(format!(
            "ACPI-only descriptor '{}' is missing an ACPI descriptor",
            rd.name
        ));
    };

    state.acpi_only_devices.push(acpi_device);
    Ok(())
}

fn effective_services(
    instance: &DriverInstance,
    disabled: &heapless::Vec<Service, 16>,
) -> Result<ServiceSet, String> {
    for service in disabled {
        if !instance.provides(*service) {
            return Err(format!(
                "driver '{}' cannot disable service '{}' because it does not provide it",
                instance.driver_name(),
                service.as_str()
            ));
        }
    }

    let mut services = instance.provided_services();
    for service in disabled {
        services.remove(*service);
    }
    Ok(services)
}

#[cfg(test)]
mod tests {
    use super::{load_parsed_board_from_rust, load_parsed_board_from_rust_with_acpi};

    #[test]
    fn ifd_bios_region_derives_flash_window_and_boot_media() {
        let parsed = load_parsed_board_from_rust(
            fstart_board_lenovo_x61::board_config(),
            fstart_board_lenovo_x61::driver_bindings(),
        )
        .unwrap();

        assert_eq!(
            parsed.config.memory.firmware_window(),
            Some((0xFFE8_0000, 0x0018_0000))
        );
        assert!(parsed.config.memory.regions.iter().any(|region| {
            region.kind == fstart_types::RegionKind::Rom
                && region.base == 0xFFE8_0000
                && region.size == 0x0018_0000
        }));
    }

    #[test]
    fn structural_nodes_do_not_gain_pseudo_services() {
        let parsed = load_parsed_board_from_rust(
            fstart_board_lenovo_x61::board_config(),
            fstart_board_lenovo_x61::driver_bindings(),
        )
        .expect("lenovo-x61 board should parse");
        let idx = parsed
            .config
            .devices
            .iter()
            .position(|dev| dev.name.as_str() == "lpc")
            .expect("fixture should contain lpc structural node");

        assert!(parsed.device_services[idx].is_empty());
        match &parsed.driver_instances[idx] {
            fstart_device_registry::DriverInstance::Structural(cfg) => {
                assert_eq!(cfg.kind, fstart_device_registry::StructuralKind::LpcBus);
            }
            other => panic!("lpc should be structural, got {other:?}"),
        }
    }

    #[test]
    fn rust_board_bindings_for_unknown_devices_are_rejected() {
        let mut bindings = fstart_board_qemu_riscv64::driver_bindings();
        let instance = bindings[0].instance.clone();
        bindings.push(fstart_device_registry::DriverBinding::new(
            "missing", instance,
        ));

        let err = match load_parsed_board_from_rust(
            fstart_board_qemu_riscv64::board_config(),
            bindings,
        ) {
            Ok(_) => panic!("unknown binding must fail"),
            Err(err) => err,
        };
        assert!(
            err.contains("driver bindings for unknown devices: missing"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn acpi_only_descriptors_are_collected_in_side_table() {
        let parsed = load_parsed_board_from_rust_with_acpi(
            fstart_board_qemu_sbsa::board_config(),
            fstart_board_qemu_sbsa::driver_bindings(),
            fstart_board_qemu_sbsa::acpi_only_devices(),
        )
        .expect("qemu-sbsa board should parse");

        assert_eq!(parsed.acpi_only_devices.len(), 2);
        assert!(matches!(
            parsed.acpi_only_devices[0],
            fstart_types::acpi::AcpiExtraDevice::Ahci(_)
        ));
        assert!(matches!(
            parsed.acpi_only_devices[1],
            fstart_types::acpi::AcpiExtraDevice::Xhci(_)
        ));
        assert_eq!(parsed.config.devices.len(), parsed.driver_instances.len());
        assert_eq!(parsed.config.devices.len(), parsed.device_services.len());
        assert_eq!(parsed.config.devices.len(), parsed.device_tree.len());
    }
}
