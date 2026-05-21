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

use fstart_device_registry::{ConstructionKind, DriverInstance, Service, StructuralConfig};
use fstart_types::acpi::AcpiExtraDevice;
use fstart_types::device::BusAddress;
use fstart_types::{
    BoardConfig, BuildMode, DeviceConfig, DeviceId, DeviceNode, MemoryMap, PayloadConfig, Platform,
    SecurityConfig, SocImageFormat, StageLayout,
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
    pub device_services: Vec<heapless::Vec<Service, 8>>,
    /// ACPI-only descriptors collected separately from runtime devices.
    /// They are also kept in `driver_instances` temporarily to preserve the
    /// lock-step flattened arrays during migration.
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

#[derive(Debug, Clone, Copy, Deserialize)]
enum StructuralKind {
    PciBridge,
    LpcBus,
    SmBus,
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
    disabled_services: heapless::Vec<Service, 8>,
    /// Typed device kind for non-runtime topology nodes.
    ///
    /// Runtime devices use `driver`; structural nodes use
    /// `kind: Structural(...)` and must not also set `driver`.
    /// ACPI-only descriptors use `kind: AcpiOnly` with an ACPI-only
    /// driver descriptor while the internal representation is migrated.
    #[serde(default)]
    kind: Option<RonDeviceKind>,
    /// Typed enum variant: `Ns16550(( base_addr: …, … ))`, `Pl011(( … ))`, etc.
    #[serde(default)]
    driver: Option<DriverInstance>,
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
    // Enable `implicit_some` so board.ron files can write `field: 42`
    // for `Option<T>` schema fields without wrapping in `Some(42)`.
    // This is forward-compatible: changing a concrete field to `Option<T>`
    // no longer breaks existing board files that use the bare value.
    let options =
        ron::Options::default().with_default_extension(ron::extensions::Extensions::IMPLICIT_SOME);
    let ron_cfg: RonBoardConfig = options
        .from_str(&contents)
        .map_err(|e| format!("failed to parse {}: {e}", path.display()))?;
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

    // Flatten each top-level device (and its children) via DFS.
    for rd in ron.devices {
        flatten_device(
            rd,
            None,
            0,
            &mut devices,
            &mut driver_instances,
            &mut device_tree,
            &mut device_services,
            &mut acpi_only_devices,
        )?;
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
    device_services: &mut Vec<heapless::Vec<Service, 8>>,
    acpi_only_devices: &mut Vec<AcpiExtraDevice>,
) -> Result<(), String> {
    let my_idx = devices.len() as DeviceId;

    // Structural nodes become explicit instances in the typed driver instance
    // table. DeviceConfig stays pure topology metadata.
    let instance = match (rd.driver, rd.kind) {
        (Some(instance), None) if instance.construction_kind() == ConstructionKind::AcpiOnly => {
            return Err(format!(
                "ACPI-only descriptor '{}' must use 'kind: AcpiOnly'",
                rd.name
            ));
        }
        (Some(instance), None) => instance,
        (None, Some(RonDeviceKind::Structural(_kind))) => {
            DriverInstance::Structural(StructuralConfig::default())
        }
        (Some(instance), Some(RonDeviceKind::AcpiOnly))
            if instance.construction_kind() == ConstructionKind::AcpiOnly =>
        {
            instance
        }
        (Some(_), Some(RonDeviceKind::AcpiOnly)) => {
            return Err(format!(
                "device '{}' uses 'kind: AcpiOnly' with a runtime driver",
                rd.name
            ));
        }
        (None, Some(RonDeviceKind::AcpiOnly)) => {
            return Err(format!(
                "ACPI-only descriptor '{}' is missing an ACPI driver descriptor",
                rd.name
            ));
        }
        (Some(_instance), Some(RonDeviceKind::Structural(_))) => {
            return Err(format!(
                "device '{}' specifies both 'driver' and structural 'kind'; choose one",
                rd.name
            ));
        }
        (None, None) => {
            return Err(format!(
                "device '{}' is missing 'driver', 'kind: Structural(...)', or 'kind: AcpiOnly'",
                rd.name
            ));
        }
    };
    let parent_name = parent_idx.map(|idx| devices[idx as usize].name.clone());

    if let Some(acpi_device) = acpi_extra_device(&instance) {
        acpi_only_devices.push(acpi_device);
    }

    let effective_services = effective_services(&instance, &rd.disabled_services)?;
    let _ = devices.push(DeviceConfig {
        name: rd.name,
        parent: parent_name,
        bus: rd.bus,
        enabled: rd.enabled,
    });
    driver_instances.push(instance);
    device_services.push(effective_services);
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
            device_services,
            acpi_only_devices,
        )?;
    }

    Ok(())
}

fn acpi_extra_device(instance: &DriverInstance) -> Option<AcpiExtraDevice> {
    match instance {
        DriverInstance::Ahci(dev) => Some(AcpiExtraDevice::Ahci(dev.clone())),
        DriverInstance::Xhci(dev) => Some(AcpiExtraDevice::Xhci(dev.clone())),
        DriverInstance::PcieRoot(dev) => Some(AcpiExtraDevice::PcieRoot(dev.clone())),
        _ => None,
    }
}

fn effective_services(
    instance: &DriverInstance,
    disabled: &heapless::Vec<Service, 8>,
) -> Result<heapless::Vec<Service, 8>, String> {
    for service in disabled {
        if !instance.provides(*service) {
            return Err(format!(
                "driver '{}' cannot disable service '{}' because it does not provide it",
                instance.driver_name(),
                service.as_str()
            ));
        }
    }

    let mut services = heapless::Vec::new();
    for service in instance.provided_services() {
        if disabled.contains(service) {
            continue;
        }
        let _ = services.push(*service);
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

    fn load_temp_board(name: &str, source: String) -> Result<(), String> {
        let path = temp_board_path(name);
        std::fs::write(&path, source).expect("write temp board");
        let load_path = path.clone();
        let result = std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(move || load_parsed_board(&load_path).map(|_| ()))
            .expect("spawn board loader")
            .join()
            .expect("join board loader");
        let _ = std::fs::remove_file(&path);
        result
    }

    fn expect_load_error(result: Result<(), String>) -> String {
        match result {
            Ok(()) => panic!("board load must fail"),
            Err(err) => err,
        }
    }

    #[test]
    fn legacy_services_field_is_rejected() {
        let source = qemu_riscv64_board_source();
        let with_legacy_services = source.replacen(
            "driver: Ns16550((",
            "services: [\"Console\"],\n            driver: Ns16550((",
            1,
        );
        assert_ne!(source, with_legacy_services, "test fixture changed");

        let err = expect_load_error(load_temp_board("legacy-services", with_legacy_services));

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
        assert!(!parsed.device_services[0].contains(&fstart_device_registry::Service::Console));
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
        let legacy_acpi_only = source.replacen(
            "kind: AcpiOnly,\n            driver: Ahci((",
            "driver: Ahci((",
            1,
        );
        assert_ne!(source, legacy_acpi_only, "test fixture changed");

        let err = expect_load_error(load_temp_board("legacy-acpi-only", legacy_acpi_only));
        assert!(
            err.contains("must use 'kind: AcpiOnly'"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn acpi_only_descriptors_are_collected_in_side_table() {
        let source = qemu_sbsa_board_source();
        let parsed = std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                let path = temp_board_path("acpi-side-table");
                std::fs::write(&path, source).unwrap();
                let parsed = load_parsed_board(&path).unwrap();
                let _ = std::fs::remove_file(&path);
                parsed
            })
            .expect("spawn ron loader worker")
            .join()
            .expect("ron loader worker panicked");

        assert_eq!(parsed.acpi_only_devices.len(), 2);
        assert!(matches!(
            parsed.acpi_only_devices[0],
            fstart_types::acpi::AcpiExtraDevice::Ahci(_)
        ));
        assert!(matches!(
            parsed.acpi_only_devices[1],
            fstart_types::acpi::AcpiExtraDevice::Xhci(_)
        ));
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
    fn device_with_driver_and_structural_kind_is_rejected() {
        let source = qemu_riscv64_board_source();
        let conflicting = source.replacen(
            "driver: Ns16550((",
            "kind: Structural(PciBridge),\n            driver: Ns16550((",
            1,
        );
        assert_ne!(source, conflicting, "test fixture changed");

        let err = expect_load_error(load_temp_board("driver-and-kind", conflicting));
        assert!(
            err.contains("specifies both 'driver' and structural 'kind'"),
            "unexpected error: {err}"
        );
    }
}
