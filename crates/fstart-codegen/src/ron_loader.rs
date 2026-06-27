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

use fstart_device_registry::{
    DriverInstance, Service, ServiceSet, StructuralConfig, StructuralKind,
};
use fstart_types::acpi::AcpiExtraDevice;
use fstart_types::device::BusAddress;
use fstart_types::{
    BoardConfig, DeviceConfig, DeviceId, DeviceNode, MemoryMap, PayloadConfig, Platform,
    SecurityConfig, SocImageFormat, StageLayout,
};

/// Serialize Rust-authored board metadata into the legacy RON shape consumed by
/// the current stage build script.
///
/// This is intentionally hosted in codegen, not in board crates. Board crates
/// should author Rust facts and typed driver instances; this function is only a
/// temporary host-side adapter while stage build.rs still accepts a RON file.
pub fn rust_board_config_to_ron(
    config: BoardConfig,
    driver_instances: Vec<DriverInstance>,
) -> Result<String, String> {
    let ron = RustBoardRon::new(config, driver_instances)?;
    let pretty = ron::ser::PrettyConfig::default();
    ron::ser::to_string_pretty(&ron, pretty)
        .map_err(|e| format!("failed to serialize Rust board metadata as transitional RON: {e}"))
}

/// Build a [`ParsedBoard`] directly from Rust-authored metadata.
///
/// This bypasses the RON parser entirely and is the preferred host API for Rust
/// board crates. The RON serializer above remains only for the current
/// `fstart-stage/build.rs` file handoff.
pub fn load_parsed_board_from_rust(
    mut config: BoardConfig,
    driver_instances: Vec<DriverInstance>,
) -> Result<ParsedBoard, String> {
    config
        .memory
        .normalize_derived_flash()
        .map_err(|err| err.to_string())?;
    if config.devices.len() != driver_instances.len() {
        return Err(format!(
            "Rust board '{}' has {} device declarations but {} driver instances",
            config.name,
            config.devices.len(),
            driver_instances.len()
        ));
    }

    let mut device_tree: Vec<DeviceNode> = Vec::with_capacity(config.devices.len());
    let mut device_services: Vec<ServiceSet> = Vec::with_capacity(config.devices.len());

    for (index, device) in config.devices.iter().enumerate() {
        let parent = match &device.parent {
            Some(parent_name) => {
                let parent_idx = config.devices[..index]
                    .iter()
                    .position(|candidate| candidate.name == *parent_name)
                    .ok_or_else(|| {
                        format!(
                            "device '{}' refers to unknown or later parent '{}'",
                            device.name, parent_name
                        )
                    })?;
                Some(parent_idx as DeviceId)
            }
            None => None,
        };
        let depth = parent
            .map(|parent_idx| device_tree[parent_idx as usize].depth.saturating_add(1))
            .unwrap_or(0);
        device_tree.push(DeviceNode { parent, depth });
        device_services.push(driver_instances[index].provided_services());
    }

    Ok(ParsedBoard {
        config,
        driver_instances,
        device_tree,
        device_services,
        acpi_only_devices: Vec::new(),
    })
}

#[derive(serde::Serialize)]
struct RustBoardRon {
    name: HString<64>,
    platform: Platform,
    memory: MemoryMap,
    devices: Vec<RustBoardRonDevice>,
    stages: StageLayout,
    security: SecurityConfig,
    payload: Option<PayloadConfig>,
    microcode: Option<fstart_types::board::MicrocodeConfig>,
    soc_image_format: SocImageFormat,
    full_flash_image: bool,
    acpi: Option<fstart_types::acpi::AcpiConfig>,
    smbios: Option<fstart_types::smbios::SmbiosConfig>,
    smm: Option<fstart_types::smm::SmmConfig>,
    boot_hart_id: u32,
}

#[derive(serde::Serialize)]
struct RustBoardRonDevice {
    name: HString<32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent: Option<HString<32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bus: Option<BusAddress>,
    enabled: bool,
    driver: DriverInstance,
}

impl RustBoardRon {
    fn new(config: BoardConfig, driver_instances: Vec<DriverInstance>) -> Result<Self, String> {
        if config.devices.len() != driver_instances.len() {
            return Err(format!(
                "Rust board '{}' has {} device declarations but {} driver instances",
                config.name,
                config.devices.len(),
                driver_instances.len()
            ));
        }
        let devices = config
            .devices
            .iter()
            .cloned()
            .zip(driver_instances)
            .map(|(device, driver)| RustBoardRonDevice {
                name: device.name,
                parent: device.parent,
                bus: device.bus,
                enabled: device.enabled,
                driver,
            })
            .collect();

        Ok(Self {
            name: config.name,
            platform: config.platform,
            memory: config.memory,
            devices,
            stages: config.stages,
            security: config.security,
            payload: config.payload,
            microcode: config.microcode,
            soc_image_format: config.soc_image_format,
            full_flash_image: config.full_flash_image,
            acpi: config.acpi,
            smbios: config.smbios,
            smm: config.smm,
            boot_hart_id: config.boot_hart_id,
        })
    }
}

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

/// Load and fully validate a board config from an in-memory RON transport.
///
/// Rust board crates use this for metadata helper output: the source of truth is
/// normal Rust code in the board package, while RON remains only the temporary
/// host-side serialization format shared with the transitional code generator.
pub fn load_parsed_board_from_str(contents: &str, source: &str) -> Result<ParsedBoard, String> {
    // Enable `implicit_some` so legacy RON and Rust-board helper output can
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
    let instance = match (rd.driver, rd.kind, rd.acpi) {
        (_, None, Some(_)) => {
            return Err(format!(
                "ACPI-only descriptor '{}' must use 'kind: AcpiOnly'",
                rd.name
            ));
        }
        (Some(instance), None, None) => instance,
        (None, Some(RonDeviceKind::Structural(kind)), None) => {
            DriverInstance::Structural(StructuralConfig { kind })
        }
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
    use super::load_parsed_board;
    use std::path::PathBuf;

    fn temp_board_path(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "fstart-ron-loader-{name}-{}-{}.ron",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        path
    }

    fn qemu_riscv64_board_source() -> String {
        let board_path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../boards/qemu-riscv64/board.ron");
        std::fs::read_to_string(board_path).expect("read qemu-riscv64 board")
    }

    fn qemu_sbsa_board_source() -> String {
        let board_path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../boards/qemu-sbsa/board.ron");
        std::fs::read_to_string(board_path).expect("read qemu-sbsa board")
    }

    fn lenovo_x61_board_source() -> String {
        let board_path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../boards/lenovo-x61/board.ron");
        std::fs::read_to_string(board_path).expect("read lenovo-x61 board")
    }

    fn foxconn_d41s_board_source() -> String {
        let board_path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../boards/foxconn-d41s/board.ron");
        std::fs::read_to_string(board_path).expect("read foxconn-d41s board")
    }

    fn load_temp_parsed(name: &str, source: String) -> Result<super::ParsedBoard, String> {
        let path = temp_board_path(name);
        std::fs::write(&path, source).expect("write temp board");
        let load_path = path.clone();
        let result = std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(move || load_parsed_board(&load_path))
            .expect("spawn board loader")
            .join()
            .expect("join board loader");
        let _ = std::fs::remove_file(&path);
        result
    }

    fn load_temp_board(name: &str, source: String) -> Result<(), String> {
        load_temp_parsed(name, source).map(|_| ())
    }

    fn expect_load_error(result: Result<(), String>) -> String {
        match result {
            Ok(()) => panic!("board load must fail"),
            Err(err) => err,
        }
    }

    #[test]
    fn unknown_car_field_is_rejected() {
        let source = lenovo_x61_board_source();
        let with_unknown_car_method = source.replacen(
            "car: Some((\n            base:",
            "car: Some((\n            method: NonEvictMode,\n            base:",
            1,
        );
        assert_ne!(source, with_unknown_car_method, "test fixture changed");

        let err = expect_load_error(load_temp_board(
            "unknown-car-field",
            with_unknown_car_method,
        ));
        assert!(
            err.contains("unknown field `method`")
                || err.contains("unknown field 'method'")
                || err.contains("Unexpected field named `method`"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn ifd_bios_region_derives_flash_window_and_boot_media() {
        let source = lenovo_x61_board_source();
        let parsed = std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(move || {
                let path = temp_board_path("ifd-derived-flash");
                std::fs::write(&path, source).expect("write temp board");
                let parsed = load_parsed_board(&path).unwrap();
                let _ = std::fs::remove_file(&path);
                parsed
            })
            .expect("spawn ron loader worker")
            .join()
            .expect("ron loader worker panicked");

        assert_eq!(
            parsed.config.memory.firmware_window(),
            Some((0xFFE8_0000, 0x0018_0000))
        );
        assert!(parsed.config.memory.regions.iter().any(|region| {
            region.kind == fstart_types::RegionKind::Rom
                && region.base == 0xFFE8_0000
                && region.size == 0x0018_0000
        }));

        let fstart_types::StageLayout::MultiStage(stages) = &parsed.config.stages else {
            panic!("lenovo-x61 should be multi-stage");
        };
        assert!(stages
            .iter()
            .all(|stage| stage.capabilities.iter().any(|cap| {
                matches!(
                    cap,
                    fstart_types::Capability::BootMedia(
                        fstart_types::BootMedium::FirmwareImage { .. }
                    )
                )
            })));
    }

    #[test]
    fn contiguous_rom_regions_derive_flash_window_and_boot_media() {
        let source = foxconn_d41s_board_source();
        let parsed = std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(move || {
                let path = temp_board_path("rom-derived-flash");
                std::fs::write(&path, source).expect("write temp board");
                let parsed = match load_parsed_board(&path) {
                    Ok(parsed) => parsed,
                    Err(err) => panic!("load foxconn-d41s board: {err}"),
                };
                let _ = std::fs::remove_file(&path);
                parsed
            })
            .expect("spawn ron loader worker")
            .join()
            .expect("ron loader worker panicked");

        assert_eq!(
            parsed.config.memory.firmware_window(),
            Some((0xFF00_0000, 0x0100_0000))
        );

        let fstart_types::StageLayout::MultiStage(stages) = &parsed.config.stages else {
            panic!("foxconn-d41s should be multi-stage");
        };
        assert!(stages.iter().all(|stage| {
            stage.capabilities.iter().any(|cap| {
                matches!(
                    cap,
                    fstart_types::Capability::BootMedia(
                        fstart_types::BootMedium::FirmwareImage { .. }
                    )
                )
            })
        }));
    }

    #[test]
    fn raw_auto_device_boot_media_is_rejected() {
        let source = qemu_riscv64_board_source();
        let with_auto_device = source.replacen(
            "BootMedia(FirmwareImage())",
            "BootMedia(AutoDevice(devices: [(name: \"mmc0\", offset: 0x2000, size: 0x800000)]))",
            1,
        );
        assert_ne!(source, with_auto_device, "test fixture changed");

        let err = expect_load_error(load_temp_board("raw-auto-device", with_auto_device));
        assert!(
            err.contains("unknown variant `AutoDevice`")
                || err.contains("unknown variant 'AutoDevice'")
                || err.contains("Unexpected variant named `AutoDevice`")
                || err.contains("Expected identifier"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn raw_memory_mapped_boot_media_is_rejected() {
        let source = qemu_riscv64_board_source();
        let with_raw_mapping = source.replacen(
            "BootMedia(FirmwareImage())",
            "BootMedia(MemoryMapped( base: 0x20000000, size: 0x02000000 ))",
            1,
        );
        assert_ne!(source, with_raw_mapping, "test fixture changed");

        let err = expect_load_error(load_temp_board("raw-memory-mapped", with_raw_mapping));
        assert!(
            err.contains("unknown variant `MemoryMapped`")
                || err.contains("unknown variant 'MemoryMapped'")
                || err.contains("Unexpected variant named `MemoryMapped`")
                || err.contains("Expected identifier"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn board_owned_service_list_field_is_rejected() {
        let source = qemu_riscv64_board_source();
        let with_board_owned_service_list = source.replacen(
            "driver: Ns16550((",
            &format!(
                "{} [\"Console\"],\n            driver: Ns16550((",
                concat!("services", ":")
            ),
            1,
        );
        assert_ne!(
            source, with_board_owned_service_list,
            "test fixture changed"
        );

        let err = expect_load_error(load_temp_board(
            "board-owned-service-list",
            with_board_owned_service_list,
        ));

        assert!(
            err.contains("unknown field `services`")
                || err.contains("unknown field 'services'")
                || err.contains("Unexpected field named `services`"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn string_disabled_services_field_is_rejected() {
        let source = qemu_riscv64_board_source();
        let with_string_disabled_console = source.replacen(
            "driver: Ns16550((",
            "disabled_services: [\"Console\"],\n            driver: Ns16550((",
            1,
        );
        assert_ne!(source, with_string_disabled_console, "test fixture changed");

        let err = expect_load_error(load_temp_board(
            "string-disabled-services",
            with_string_disabled_console,
        ));

        assert!(
            err.contains("Expected identifier")
                || err.contains("Expected enum")
                || err.contains("invalid type"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn disabled_services_removes_single_service() {
        let source = qemu_riscv64_board_source();
        let with_disabled_console = source.replacen(
            "driver: Ns16550((",
            "disabled_services: [Console],\n            driver: Ns16550((",
            1,
        );
        assert_ne!(source, with_disabled_console, "test fixture changed");

        let parsed = std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                let path = temp_board_path("disabled-console");
                std::fs::write(&path, with_disabled_console).unwrap();
                let parsed = load_parsed_board(&path).unwrap();
                let _ = std::fs::remove_file(&path);
                parsed
            })
            .expect("spawn ron loader worker")
            .join()
            .expect("ron loader worker panicked");

        assert!(parsed.driver_instances[0].provides(fstart_device_registry::Service::Console));
        assert!(!parsed.device_services[0].contains(fstart_device_registry::Service::Console));
    }

    #[test]
    fn runtime_device_without_driver_is_rejected() {
        let source = qemu_riscv64_board_source();
        let missing_driver = source.replacen(
            r#"        (
            name: "uart0",
            driver: Ns16550((
                regs: Mmio(base: 0x10000000, reg_shift: 0, reg_width: 0),
                clock_freq: 3686400,
                baud_rate: 115200,
            )),
        ),"#,
            r#"        (
            name: "uart0",
        ),"#,
            1,
        );
        assert_ne!(source, missing_driver, "test fixture changed");

        let err = expect_load_error(load_temp_board("missing-driver", missing_driver));
        assert!(
            err.contains("missing 'driver', 'kind: Structural(...)', or 'kind: AcpiOnly'"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn acpi_only_descriptor_requires_explicit_kind() {
        let source = qemu_sbsa_board_source();
        let missing_kind = source.replacen(
            "kind: AcpiOnly,\n            acpi: Ahci((",
            "acpi: Ahci((",
            1,
        );
        assert_ne!(source, missing_kind, "test fixture changed");

        let err = expect_load_error(load_temp_board("acpi-only-missing-kind", missing_kind));
        assert!(
            err.contains("must use 'kind: AcpiOnly'"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn acpi_only_descriptors_are_collected_in_side_table() {
        let parsed = load_temp_parsed("acpi-side-table", qemu_sbsa_board_source())
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
        assert!(parsed
            .config
            .devices
            .iter()
            .all(|dev| dev.name.as_str() != "ahci0" && dev.name.as_str() != "xhci0"));
        assert_eq!(parsed.config.devices.len(), parsed.driver_instances.len());
        assert_eq!(parsed.config.devices.len(), parsed.device_services.len());
        assert_eq!(parsed.config.devices.len(), parsed.device_tree.len());
    }

    #[test]
    fn acpi_only_kind_with_runtime_driver_is_rejected() {
        let source = qemu_riscv64_board_source();
        let conflicting = source.replacen(
            "driver: Ns16550((",
            "kind: AcpiOnly,\n            driver: Ns16550((",
            1,
        );
        assert_ne!(source, conflicting, "test fixture changed");

        let err = expect_load_error(load_temp_board("acpi-only-runtime", conflicting));
        assert!(
            err.contains("uses 'kind: AcpiOnly' with a runtime driver"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn structural_nodes_do_not_gain_pseudo_services() {
        let parsed = load_temp_parsed("structural-services", foxconn_d41s_board_source())
            .expect("foxconn board should parse");
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
    fn device_with_driver_and_topology_kind_is_rejected() {
        let source = qemu_riscv64_board_source();
        let conflicting = source.replacen(
            "driver: Ns16550((",
            "kind: Structural(PciBridge),\n            driver: Ns16550((",
            1,
        );
        assert_ne!(source, conflicting, "test fixture changed");

        let err = expect_load_error(load_temp_board("driver-and-kind", conflicting));
        assert!(
            err.contains("specifies both runtime/ACPI descriptor and structural 'kind'"),
            "unexpected error: {err}"
        );
    }
}
