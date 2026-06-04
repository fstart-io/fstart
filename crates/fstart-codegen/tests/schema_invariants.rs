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
fn board_ron_does_not_declare_legacy_services() {
    let root = repo_root();
    let offenders: Vec<_> = files_under(&root, "boards", |p| {
        p.extension().is_some_and(|e| e == "ron")
    })
    .into_iter()
    .filter(|path| {
        fs::read_to_string(path)
            .expect("read board")
            .contains("services:")
    })
    .collect();

    assert!(
        offenders.is_empty(),
        "board RON files must not declare legacy services: {offenders:?}"
    );
}

#[test]
fn production_schema_has_no_structural_sentinel() {
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
            if text.contains("\"_structural\"") {
                offenders.push(path);
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "production schema must not use _structural sentinel: {offenders:?}"
    );
}

#[test]
fn device_config_has_no_services_field() {
    let root = repo_root();
    let device_rs = root.join("crates/fstart-types/src/device.rs");
    let text = fs::read_to_string(device_rs).expect("read device schema");

    assert!(
        !text.contains("pub services"),
        "DeviceConfig must not grow a board-owned services field"
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
