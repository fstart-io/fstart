//! Regression tests for the Rust-owned driver service schema.

use std::fs;
use std::path::{Path, PathBuf};

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
