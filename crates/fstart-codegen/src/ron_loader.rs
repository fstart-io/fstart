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
use fstart_types::memory::{FlashLayout, MemoryRegion, RegionKind};
use fstart_types::{
    BoardConfig, BootMedium, BuildMode, Capability, DeviceConfig, DeviceId, DeviceNode, MemoryMap,
    PayloadConfig, Platform, SecurityConfig, SocImageFormat, StageLayout,
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
    pub device_services: Vec<heapless::Vec<Service, 16>>,
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
#[serde(deny_unknown_fields)]
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
    disabled_services: heapless::Vec<Service, 16>,
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
    let mut ron_cfg: RonBoardConfig = options
        .from_str(&contents)
        .map_err(|e| format!("failed to parse {}: {e}", path.display()))?;
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

// -----------------------------------------------------------------------
// Normalization — derive redundant board facts and expand shorthands
// -----------------------------------------------------------------------

/// Normalize deserialized RON before flattening.
///
/// Board files should not have to repeat the firmware image window in every
/// place that consumes it.  Intel IFD boards already describe the BIOS region
/// in `flash_layout`, so derive `memory.flash_base/flash_size` and the linker
/// ROM region from that single source when they are omitted.  Likewise,
/// `BootMedia(MemoryMappedFlash(...))` is a stage-local shorthand for the
/// board's firmware-image window.
fn normalize_ron_config(ron: &mut RonBoardConfig) -> Result<(), String> {
    normalize_memory_flash(&mut ron.memory)?;
    resolve_stage_boot_media(&mut ron.stages, &ron.memory)
}

fn normalize_memory_flash(memory: &mut MemoryMap) -> Result<(), String> {
    let Some(FlashLayout::IntelIfd(layout)) = &memory.flash_layout else {
        return Ok(());
    };
    let bios = layout
        .bios_region()
        .ok_or_else(|| "Intel IFD flash_layout requires a BIOS region".to_string())?;
    let expected_base = layout.base + u64::from(bios.offset);
    let expected_size = u64::from(bios.size);

    match (memory.flash_base, memory.flash_size) {
        (Some(base), Some(size)) if base == expected_base && size == expected_size => {}
        (None, None) => {
            memory.flash_base = Some(expected_base);
            memory.flash_size = Some(expected_size);
        }
        (base, size) => {
            return Err(format!(
                "memory.flash_base/flash_size must describe the Intel IFD BIOS region: \
                 expected base={expected_base:#x} size={expected_size:#x}, got base={base:?} size={size:?}"
            ));
        }
    }

    ensure_ifd_bios_rom_region(memory, expected_base, expected_size)
}

fn ensure_ifd_bios_rom_region(
    memory: &mut MemoryMap,
    expected_base: u64,
    expected_size: u64,
) -> Result<(), String> {
    let expected_end = expected_base.saturating_add(expected_size);
    let mut has_exact = false;
    for region in &memory.regions {
        if region.kind != RegionKind::Rom {
            continue;
        }
        if region.base == expected_base && region.size == expected_size {
            has_exact = true;
            break;
        }
        let region_end = region.base.saturating_add(region.size);
        if region.base < expected_end && expected_base < region_end {
            return Err(format!(
                "ROM memory region '{}' overlaps the Intel IFD BIOS region but does not match it: \
                 expected base={expected_base:#x} size={expected_size:#x}, got base={:#x} size={:#x}",
                region.name, region.base, region.size
            ));
        }
    }

    if has_exact {
        return Ok(());
    }

    memory
        .regions
        .push(MemoryRegion {
            name: HString::try_from("flash").map_err(|_| "failed to build flash region name")?,
            base: expected_base,
            size: expected_size,
            kind: RegionKind::Rom,
        })
        .map_err(|_| "memory.regions is full; cannot add Intel IFD BIOS ROM region".to_string())
}

fn resolve_stage_boot_media(stages: &mut StageLayout, memory: &MemoryMap) -> Result<(), String> {
    match stages {
        StageLayout::Monolithic(mono) => {
            resolve_capability_boot_media(&mut mono.capabilities, memory)
        }
        StageLayout::MultiStage(stages) => {
            for stage in stages {
                resolve_capability_boot_media(&mut stage.capabilities, memory)?;
            }
            Ok(())
        }
    }
}

fn resolve_capability_boot_media(
    capabilities: &mut heapless::Vec<Capability, 16>,
    memory: &MemoryMap,
) -> Result<(), String> {
    for capability in capabilities {
        let Capability::BootMedia(medium) = capability else {
            continue;
        };
        if let BootMedium::MemoryMappedFlash { ram_copy_addr } = medium {
            let (base, size) = match (memory.flash_base, memory.flash_size) {
                (Some(base), Some(size)) => (base, size),
                _ => {
                    return Err(
                        "BootMedia(MemoryMappedFlash(...)) requires memory.flash_base/flash_size \
                         or an Intel IFD BIOS flash_layout"
                            .to_string(),
                    );
                }
            };
            *medium = BootMedium::MemoryMapped {
                base,
                size,
                ram_copy_addr: *ram_copy_addr,
            };
        }
    }
    Ok(())
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

struct FlattenState<'a> {
    devices: &'a mut heapless::Vec<DeviceConfig, 32>,
    driver_instances: &'a mut Vec<DriverInstance>,
    device_tree: &'a mut Vec<DeviceNode>,
    device_services: &'a mut Vec<heapless::Vec<Service, 16>>,
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
    let my_idx = state.devices.len() as DeviceId;

    // Structural nodes become explicit instances in the typed driver instance
    // table. DeviceConfig stays pure topology metadata.
    let structural_kind = match rd.kind {
        Some(RonDeviceKind::Structural(kind)) => Some(kind),
        _ => None,
    };
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
    let parent_name = parent_idx.map(|idx| state.devices[idx as usize].name.clone());

    if let Some(acpi_device) = acpi_extra_device(&instance) {
        state.acpi_only_devices.push(acpi_device);
    }

    let effective_services = effective_services(&instance, structural_kind, &rd.disabled_services)?;
    let _ = state.devices.push(DeviceConfig {
        name: rd.name,
        parent: parent_name,
        bus: rd.bus,
        enabled: rd.enabled,
    });
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
    structural_kind: Option<StructuralKind>,
    disabled: &heapless::Vec<Service, 16>,
) -> Result<heapless::Vec<Service, 16>, String> {
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
    if let Some(service) = structural_service(structural_kind) {
        let _ = services.push(service);
    }
    for service in instance.provided_services() {
        if disabled.contains(service) {
            continue;
        }
        let _ = services.push(*service);
    }
    Ok(services)
}

fn structural_service(kind: Option<StructuralKind>) -> Option<Service> {
    match kind {
        Some(StructuralKind::PciBridge) => Some(Service::PciBridge),
        Some(StructuralKind::LpcBus) => Some(Service::LpcBus),
        Some(StructuralKind::SmBus) => Some(Service::SmBus),
        None => None,
    }
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

        assert_eq!(parsed.config.memory.flash_base, Some(0xFFE8_0000));
        assert_eq!(parsed.config.memory.flash_size, Some(0x0018_0000));
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
                    fstart_types::Capability::BootMedia(fstart_types::BootMedium::MemoryMapped {
                        base: 0xFFE8_0000,
                        size: 0x0018_0000,
                        ..
                    })
                )
            })));
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
