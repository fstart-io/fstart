//! Regression tests for the Rust-owned driver service schema.

use std::fs;
use std::path::{Path, PathBuf};

use fstart_codegen::ron_loader::load_parsed_board;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crate is under crates/fstart-codegen")
        .to_path_buf()
}

fn files_under(root: &Path, rel: &str, pred: fn(&Path) -> bool) -> Vec<PathBuf> {
    fn walk(dir: &Path, pred: fn(&Path) -> bool, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).expect("read dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                walk(&path, pred, out);
            } else if pred(&path) {
                out.push(path);
            }
        }
    }

    let mut out = Vec::new();
    walk(&root.join(rel), pred, &mut out);
    out
}

#[test]
fn board_gen_modules_stay_small() {
    let root = repo_root();
    let board_gen = root.join("crates/fstart-codegen/src/stage_gen/board_gen");
    let mut oversized = Vec::new();

    for path in files_under(&board_gen, "", |p| {
        p.extension().is_some_and(|ext| ext == "rs")
    }) {
        let text = fs::read_to_string(&path).expect("read board_gen module");
        let lines = text.lines().count();
        if lines > 700 {
            oversized.push(format!("{} ({lines} lines)", path.display()));
        }
    }

    assert!(
        oversized.is_empty(),
        "board_gen modules should stay below the Phase 6 size budget: {oversized:?}"
    );
}

#[test]
fn board_gen_production_code_has_no_todo_stubs() {
    let root = repo_root();
    let board_gen = root.join("crates/fstart-codegen/src/stage_gen/board_gen");
    let mut offenders = Vec::new();

    for path in files_under(&board_gen, "", |p| {
        p.extension().is_some_and(|ext| ext == "rs")
    }) {
        if path
            .components()
            .any(|component| component.as_os_str() == "tests")
        {
            continue;
        }
        let text = fs::read_to_string(&path).expect("read board_gen module");
        if text.contains(concat!("todo", "!(")) {
            offenders.push(path);
        }
    }

    assert!(
        offenders.is_empty(),
        "generated board adapter code must use explicit validation or unreachable bodies, not todo! stubs: {offenders:?}"
    );
}

#[test]
fn serde_deserialize_structs_deny_unknown_fields() {
    let root = repo_root();
    let mut offenders = Vec::new();

    for path in files_under(&root, "crates", |p| {
        p.extension().is_some_and(|e| e == "rs")
    }) {
        let text = fs::read_to_string(&path).expect("read source");
        let lines: Vec<_> = text.lines().collect();
        for (idx, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            if !trimmed.starts_with("#[derive") || !trimmed.contains("Deserialize") {
                continue;
            }

            let mut lookahead = idx + 1;
            let mut has_deny_unknown_fields = false;
            while let Some(next) = lines.get(lookahead).map(|line| line.trim()) {
                if next.contains("deny_unknown_fields") {
                    has_deny_unknown_fields = true;
                }
                if next.is_empty()
                    || next.starts_with("#[")
                    || next.starts_with("///")
                    || next.starts_with("//")
                {
                    lookahead += 1;
                    continue;
                }
                break;
            }

            let Some(item) = lines.get(lookahead).map(|line| line.trim()) else {
                continue;
            };
            if item.contains("struct ") && !has_deny_unknown_fields {
                offenders.push(format!("{}:{}", path.display(), idx + 1));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "Deserialize structs must use #[serde(deny_unknown_fields)]: {offenders:?}"
    );
}

#[test]
fn all_board_ron_files_parse() {
    let root = repo_root();
    let boards: Vec<_> = files_under(&root, "boards", |p| {
        p.file_name().is_some_and(|n| n == "board.ron")
    });

    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            for board in boards {
                load_parsed_board(&board)
                    .unwrap_or_else(|e| panic!("failed to parse {}: {e}", board.display()));
            }
        })
        .expect("spawn board parser thread")
        .join()
        .expect("board parser thread panicked");
}

#[test]
fn parsed_runtime_device_tables_exclude_acpi_only_descriptors() {
    let root = repo_root();
    let boards: Vec<_> = files_under(&root, "boards", |p| {
        p.file_name().is_some_and(|n| n == "board.ron")
    });

    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            for board in boards {
                let parsed = load_parsed_board(&board)
                    .unwrap_or_else(|e| panic!("failed to parse {}: {e}", board.display()));
                assert_eq!(parsed.config.devices.len(), parsed.driver_instances.len());
                assert_eq!(parsed.config.devices.len(), parsed.device_services.len());
                assert_eq!(parsed.config.devices.len(), parsed.device_tree.len());
                assert!(
                    parsed
                        .driver_instances
                        .iter()
                        .all(|inst| inst.has_runtime_driver() || inst.meta().name == "structural"),
                    "ACPI-only descriptors must stay outside runtime device tables: {}",
                    board.display()
                );
            }
        })
        .expect("spawn board parser thread")
        .join()
        .expect("board parser thread panicked");
}

#[test]
fn board_ron_acpi_only_descriptors_use_acpi_field() {
    let root = repo_root();
    let mut offenders = Vec::new();

    for path in files_under(&root, "boards", |p| {
        p.file_name().is_some_and(|n| n == "board.ron")
    }) {
        let text = fs::read_to_string(&path).expect("read board");
        let lines: Vec<_> = text.lines().collect();
        for (idx, line) in lines.iter().enumerate() {
            if !line.contains("kind: AcpiOnly") {
                continue;
            }
            let window = lines
                .iter()
                .skip(idx + 1)
                .take(4)
                .copied()
                .collect::<Vec<_>>()
                .join("\n");
            if window.contains("driver:") || !window.contains("acpi:") {
                offenders.push(format!("{}:{}", path.display(), idx + 1));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "ACPI-only RON entries must use `acpi:`, not runtime `driver:`: {offenders:?}"
    );
}

#[test]
fn board_ron_does_not_declare_board_owned_service_list() {
    let root = repo_root();
    let offenders: Vec<_> = files_under(&root, "boards", |p| {
        p.extension().is_some_and(|e| e == "ron")
    })
    .into_iter()
    .filter(|path| {
        fs::read_to_string(path)
            .expect("read board")
            .contains(concat!("services", ":"))
    })
    .collect();

    assert!(
        offenders.is_empty(),
        "board RON files must not declare board-owned service lists: {offenders:?}"
    );
}

#[test]
fn production_schema_encodes_topology_without_driver_name_marker() {
    let root = repo_root();
    let mut offenders = Vec::new();
    for rel in [
        "boards",
        "crates/fstart-types/src",
        "crates/fstart-codegen/src",
    ] {
        for path in files_under(&root, rel, |p| {
            matches!(p.extension().and_then(|e| e.to_str()), Some("rs" | "ron"))
        }) {
            let text = fs::read_to_string(&path).expect("read source");
            if text.contains(concat!("\"", "_", "structural", "\"")) {
                offenders.push(path);
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "production schema must not encode structural topology as a driver name: {offenders:?}"
    );
}

#[test]
fn device_config_has_no_services_field() {
    let root = repo_root();
    let device_rs = root.join("crates/fstart-types/src/device.rs");
    let text = fs::read_to_string(device_rs).expect("read device schema");

    assert!(
        !text.contains(concat!("pub ", "services")),
        "DeviceConfig must not grow a board-owned services field"
    );
}

#[test]
fn ron_loader_production_schema_has_no_board_owned_service_list_field() {
    let root = repo_root();
    let ron_loader = root.join("crates/fstart-codegen/src/ron_loader.rs");
    let text = fs::read_to_string(ron_loader).expect("read RON loader");
    let production = text
        .split("#[cfg(test)]")
        .next()
        .expect("RON loader should have production section before tests");

    let offenders: Vec<_> = production
        .lines()
        .enumerate()
        .filter_map(|(idx, line)| {
            let trimmed = line.trim_start();
            (trimmed.starts_with(concat!("services", ":"))
                || trimmed.starts_with(concat!("pub ", "services")))
            .then_some(idx + 1)
        })
        .collect();

    assert!(
        offenders.is_empty(),
        "production RON schema must not restore board-owned service-list field: {offenders:?}"
    );
}

#[test]
fn driver_instance_has_no_acpi_only_pseudo_devices() {
    let root = repo_root();
    let registry_rs = root.join("crates/fstart-device-registry/src/lib.rs");
    let text = fs::read_to_string(registry_rs).expect("read registry");

    for variant in [
        "Ahci(fstart_types::acpi::AcpiAhciDevice)",
        "Xhci(fstart_types::acpi::AcpiXhciDevice)",
        "PcieRoot(fstart_types::acpi::AcpiPcieRootDevice)",
    ] {
        assert!(
            !text.contains(variant),
            "ACPI-only descriptors must not be DriverInstance pseudo-devices: {variant}"
        );
    }
    assert!(
        !text.contains("ConstructionKind::AcpiOnly"),
        "ACPI-only descriptors must stay outside DriverInstance construction kinds"
    );
}

#[test]
fn service_enum_excludes_topology_variants() {
    let root = repo_root();
    let registry_rs = root.join("crates/fstart-device-registry/src/lib.rs");
    let text = fs::read_to_string(registry_rs).expect("read registry");
    let service_enum = text
        .split("pub enum Service {")
        .nth(1)
        .and_then(|tail| tail.split("impl Service").next())
        .expect("registry should define Service enum before impl Service");

    for variant in ["PciBridge", "LpcBus", "SmBus"] {
        assert!(
            !service_enum.contains(variant),
            "structural topology must not re-enter Service enum: {variant}"
        );
    }
}

#[test]
fn codegen_does_not_compare_service_string_literals() {
    let root = repo_root();
    let service_names = [
        "Console",
        "BlockDevice",
        "ClockController",
        "MemoryController",
        "PciRootBus",
        "AcpiTableProvider",
        "MemoryDetector",
        "SmmOps",
        "SystemManagementBus",
        "SmBus",
    ];
    let mut offenders = Vec::new();
    for path in files_under(&root, "crates/fstart-codegen/src", |p| {
        matches!(p.extension().and_then(|e| e.to_str()), Some("rs"))
    }) {
        let text = fs::read_to_string(&path).expect("read source");
        if service_names.iter().any(|service| {
            text.contains(&format!("== \"{service}\""))
                || text.contains(&format!("!= \"{service}\""))
        }) {
            offenders.push(path);
        }
    }

    assert!(
        offenders.is_empty(),
        "codegen must use typed Service values, not service string comparisons: {offenders:?}"
    );
}
