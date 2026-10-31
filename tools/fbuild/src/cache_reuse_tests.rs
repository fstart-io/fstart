//! Compile two different board packages and compare actual Cargo library
//! receipts, including an opt-in cfg consumer. This fails if link-only flags
//! or a board's check-cfg vocabulary leak back into library fingerprints.
use crate::{board_manifest, build_board, compiler_cfg, selection::Selection};
use fstart_image_build::plan::BuildSelection;
use std::{collections::BTreeMap, fs, path::Path, process::Command};

#[test]
fn first_party_outputs_are_fresh_across_boards_with_different_linker_scripts() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    // Keep direct test-executable runs on the project's nightly too.
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../rust-toolchain.toml"),
        root.join("rust-toolchain.toml"),
    )
    .unwrap();
    fs::write(root.join("Cargo.toml"), "[workspace]\nresolver='3'\nmembers=['boards/test/one','boards/test/two','crates/common','crates/selected']\n").unwrap();
    for (name, source) in [
        ("common", "pub fn value() -> u8 { 7 }"),
        (
            "selected",
            "#[cfg(fstart_stage_env=\"monolithic\")] pub fn value() -> u8 { 9 }\n#[cfg(fstart_stage_env=\"smm\")] pub fn value() -> u8 { 11 }",
        ),
    ] {
        let dir = root.join("crates").join(name);
        fs::create_dir_all(dir.join("src")).unwrap();
        let schema = if name == "selected" {
            "[package.metadata.fstart.compiler-cfg]\nfstart_stage_env=['monolithic','smm']\n"
        } else {
            ""
        };
        fs::write(
            dir.join("Cargo.toml"),
            format!("[package]\nname='{name}'\nversion='0.1.0'\nedition='2024'\n{schema}"),
        )
        .unwrap();
        fs::write(dir.join("src/lib.rs"), source).unwrap();
    }
    for name in ["one", "two"] {
        let dir = root.join("boards/test").join(name);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("Cargo.toml"), format!("[package]\nname='cache-{name}'\nversion='0.1.0'\nedition='2024'\n[package.metadata.fstart]\nschema=1\nboard='cache-{name}'\nstage-bin='fstart-stage'\n[dependencies]\ncommon={{path='../../../crates/common'}}\nselected={{path='../../../crates/selected'}}\n[[bin]]\nname='fstart-stage'\npath='src/main.rs'\n")).unwrap();
        fs::write(
            dir.join("src/main.rs"),
            "fn main() { println!(\"{}\", common::value() + selected::value()); }",
        )
        .unwrap();
    }
    let lock = Command::new("cargo")
        .current_dir(root)
        .args(["generate-lockfile", "--offline"])
        .output()
        .unwrap();
    assert!(
        lock.status.success(),
        "{}",
        String::from_utf8_lossy(&lock.stderr)
    );
    let version = Command::new("rustc").arg("-vV").output().unwrap();
    let version = String::from_utf8(version.stdout).unwrap();
    let host = version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .unwrap();
    let metadata = Command::new("cargo")
        .current_dir(root)
        .args(["metadata", "--offline", "--no-deps", "--format-version=1"])
        .output()
        .unwrap();
    assert!(
        metadata.status.success(),
        "{}",
        String::from_utf8_lossy(&metadata.stderr)
    );
    let schemas =
        compiler_cfg::packages(&serde_json::from_slice(&metadata.stdout).unwrap()).unwrap();
    let mut first = None;
    let mut last_unit = None;
    for (index, name) in ["one", "two"].into_iter().enumerate() {
        let board = board_manifest::find(root, &format!("cache-{name}")).unwrap();
        build_board::prepare_selected_board_workspace(root, &board).unwrap();
        let mut unit = fstart_platform_qemu::host::resolve(
            if index == 0 {
                fstart_platform_qemu::facts::VirtMachine::Riscv64
            } else {
                fstart_platform_qemu::facts::VirtMachine::SifiveU
            },
            BuildSelection {
                payload: Some("halt".into()),
                ..Default::default()
            },
        )
        .unwrap()
        .units
        .remove(0);
        unit.target = host.into();
        unit.features.clear();
        unit.rustflags.clear();
        unit.build_std = None;
        unit.linker_script = Some(format!(
            "SECTIONS {{ .fstart_test : {{ BYTE({index}) }} }} INSERT AFTER .text;"
        ));
        if index == 1 {
            unit.cfg_schema.entries.push("another-board-entry".into());
        }
        last_unit = Some(unit.clone());
        let selection = Selection::prepare_unit(
            root,
            &board,
            &unit,
            &schemas,
            root.join("selections").join(name),
            "{}",
            BTreeMap::new(),
            &[],
            true,
        )
        .unwrap();
        selection.validate_host_units(root).unwrap();
        assert!(!selection.package_flags.contains_key("common"));
        let output = selection.command(true).current_dir(root).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let receipts: BTreeMap<String, serde_json::Value> = output
            .stdout
            .split(|&b| b == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice::<serde_json::Value>(line).unwrap())
            .filter(|row| row["reason"] == "compiler-artifact")
            .map(|row| (row["target"]["name"].as_str().unwrap().into(), row))
            .collect();
        if let Some(previous) = &first {
            let previous: &BTreeMap<String, serde_json::Value> = previous;
            for library in ["common", "selected"] {
                assert_eq!(
                    receipts[library]["filenames"], previous[library]["filenames"],
                    "{library} did not share its output path"
                );
                assert_eq!(
                    receipts[library]["fresh"], true,
                    "{library} was rebuilt for the second board"
                );
                assert!(
                    receipts[library]["filenames"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|p| Path::new(p.as_str().unwrap())
                            .extension()
                            .is_some_and(|ext| ext == "rlib"))
                );
            }
        }
        first = Some(receipts);
    }
    // A consumed axis must invalidate that consumer, but not a disjoint lib.
    let board = board_manifest::find(root, "cache-two").unwrap();
    let mut unit = last_unit.unwrap();
    unit.environment = "smm".into();
    let changed = Selection::prepare_unit(
        root,
        &board,
        &unit,
        &schemas,
        root.join("selections/changed"),
        "{}",
        BTreeMap::new(),
        &[],
        true,
    )
    .unwrap();
    changed.validate_host_units(root).unwrap();
    let output = changed.command(true).current_dir(root).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let receipts: BTreeMap<String, serde_json::Value> = output
        .stdout
        .split(|&b| b == b'\n')
        .filter_map(|line| serde_json::from_slice::<serde_json::Value>(line).ok())
        .filter(|row| row["reason"] == "compiler-artifact")
        .map(|row| (row["target"]["name"].as_str().unwrap().into(), row))
        .collect();
    let previous = first.unwrap();
    assert_eq!(receipts["selected"]["fresh"], false);
    assert_ne!(
        receipts["selected"]["filenames"],
        previous["selected"]["filenames"]
    );
    assert_eq!(receipts["common"]["fresh"], true);
    assert_eq!(
        receipts["common"]["filenames"],
        previous["common"]["filenames"]
    );

    // Cargo's package profile overrides must never pass target selectors into
    // an opted-in dependency's host build, even when also a normal dependency.
    fs::write(
        board.dir.join("build.rs"),
        "fn main() { let _ = selected::value(); }",
    )
    .unwrap();
    let manifest = board.dir.join("Cargo.toml");
    let mut source = fs::read_to_string(&manifest).unwrap();
    source.push_str("\n[build-dependencies]\nselected={path='../../../crates/selected'}\n");
    fs::write(manifest, source).unwrap();
    let error = changed.validate_host_units(root).unwrap_err();
    assert!(
        error.contains("selected appears in a host compiler unit"),
        "{error}"
    );
}
