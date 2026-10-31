use super::Selection;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn platform_cfg_schema_accepts_new_vocabulary_but_not_selected_typos() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let board = crate::board_manifest::find(root, "qemu-aarch64").unwrap();
    let scratch =
        Scratch(std::env::temp_dir().join(format!("fstart cfg schema {}", std::process::id())));
    let mut unit = fstart_platform_qemu::host::resolve(
        fstart_platform_qemu::facts::VirtMachine::Aarch64,
        fstart_image_build::plan::BuildSelection {
            payload: Some("halt".into()),
            ..Default::default()
        },
    )
    .unwrap()
    .units
    .remove(0);
    unit.entry = "novel-reset".into();
    unit.environment = "novel-memory".into();
    unit.cfg_schema.entries = vec!["novel-reset".into()];
    unit.cfg_schema.environments = vec!["novel-memory".into()];
    unit.linker_script = None;
    let selection = Selection::prepare_unit(
        root,
        &board,
        &unit,
        &Default::default(),
        scratch.0.clone(),
        "{}",
        BTreeMap::new(),
        &[],
        true,
    )
    .unwrap();
    let source = scratch.0.join("probe.rs");
    fs::write(&source, "#[cfg(all(fstart_entry=\"novel-reset\",fstart_stage_env=\"novel-memory\",fstart_payload=\"halt\"))] const SELECTED: u8 = 1; pub const CHECK: u8 = SELECTED;").unwrap();
    let result = std::process::Command::new("rustc")
        .args(["--crate-type=lib", "--emit=metadata", "-Dunexpected_cfgs"])
        .args(&selection.flags)
        .args(member_flags(&selection))
        .arg(&source)
        .arg("-o")
        .arg(scratch.0.join("probe.rmeta"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    unit.entry = "novel-rest".into();
    assert!(unit.check_cfg_flags().unwrap_err().contains("schema"));
    assert!(
        Selection::prepare_unit(
            root,
            &board,
            &unit,
            &Default::default(),
            scratch.0.clone(),
            "{}",
            BTreeMap::new(),
            &[],
            true
        )
        .is_err()
    );
}

#[test]
fn linker_paths_are_absent_from_package_profile_flags() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let scratch = Scratch(
        std::env::temp_dir().join(format!("fstart cross-board reuse {}", std::process::id())),
    );
    let unit = fstart_platform_qemu::host::resolve(
        fstart_platform_qemu::facts::VirtMachine::Riscv64,
        fstart_image_build::plan::BuildSelection {
            payload: Some("halt".into()),
            ..Default::default()
        },
    )
    .unwrap()
    .units
    .remove(0);
    let prepare = |id: &str| {
        let board = crate::board_manifest::find(&root, id).unwrap();
        Selection::prepare_unit(
            &root,
            &board,
            &unit,
            &Default::default(),
            scratch.0.join(id),
            "{}",
            BTreeMap::new(),
            &[],
            true,
        )
        .unwrap()
    };
    let first = prepare("qemu-riscv64");
    let second = prepare("sifive-unmatched");
    assert_eq!(
        first.target_dir(&root),
        root.join("target/fstart-build/cargo")
    );
    assert_eq!(first.target_dir(&root), second.target_dir(&root));
    assert_eq!(first.flags, second.flags);
    for selection in [&first, &second] {
        assert!(
            selection
                .package_flags
                .values()
                .flatten()
                .all(|flag| !flag.contains("link-arg"))
        );
        assert_eq!(selection.command(true).get_args().next().unwrap(), "rustc");
    }
    for selection in [&first, &second] {
        let text = fs::read_to_string(selection.directory.join("member-rustflags.toml")).unwrap();
        let config: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(
            config["profile"]["release"]["rustflags"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            config["profile"]["release"]["build-override"]["rustflags"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
    }
}

#[test]
fn ambient_artifact_environment_isolation() {
    if std::env::var_os("SELECTION_ENV_PROBE").is_none() {
        // Subprocess isolation avoids unsafe process-global environment mutation
        // while the test runner is executing other tests in parallel.
        for unknown in [false, true] {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child
                .args([
                    "--exact",
                    "selection::tests::ambient_artifact_environment_isolation",
                    "--nocapture",
                ])
                .env("SELECTION_ENV_PROBE", "1")
                .env("FSTART_FIXTURE_IMAGE", "unrecorded-by-consumer")
                .env("AUX_INPUT", "another-leak");
            if unknown {
                child.env("FSTART_UNKNOWN_PRODUCER", "leak");
            }
            assert!(child.status().unwrap().success());
        }
        return;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let board = crate::board_manifest::find(root, "qemu-aarch64").unwrap();
    let scratch =
        Scratch(std::env::temp_dir().join(format!("fstart env isolation {}", std::process::id())));
    let unit = fstart_platform_qemu::host::resolve(
        fstart_platform_qemu::facts::VirtMachine::Aarch64,
        fstart_image_build::plan::BuildSelection {
            payload: Some("halt".into()),
            ..Default::default()
        },
    )
    .unwrap()
    .units
    .remove(0);
    let keys = vec!["FSTART_FIXTURE_IMAGE".into(), "AUX_INPUT".into()];
    let prepare = |env| {
        Selection::prepare_unit(
            root,
            &board,
            &unit,
            &Default::default(),
            scratch.0.clone(),
            "{}",
            env,
            &keys,
            true,
        )
    };
    if std::env::var_os("FSTART_UNKNOWN_PRODUCER").is_some() {
        assert!(
            prepare(BTreeMap::new())
                .err()
                .unwrap()
                .contains("unrecorded ambient")
        );
        return;
    }
    let unbound = prepare(BTreeMap::new()).unwrap();
    let mut command = std::process::Command::new("env");
    unbound.apply_environment(&mut command);
    let output = String::from_utf8(command.output().unwrap().stdout).unwrap();
    assert!(!output.contains("FSTART_FIXTURE_IMAGE=") && !output.contains("AUX_INPUT="));
    assert_eq!(unbound.check_command().unwrap()[1], "editor-check");
    let saved: serde_json::Value =
        serde_json::from_slice(&fs::read(unbound.directory.join("editor-selection.json")).unwrap())
            .unwrap();
    assert_eq!(
        saved["removed_environment"],
        serde_json::json!(["FSTART_FIXTURE_IMAGE", "AUX_INPUT"])
    );
    let bound = prepare(BTreeMap::from([(
        "FSTART_FIXTURE_IMAGE".into(),
        "recorded".into(),
    )]))
    .unwrap();
    let mut command = std::process::Command::new("env");
    bound.apply_environment(&mut command);
    let output = String::from_utf8(command.output().unwrap().stdout).unwrap();
    assert!(output.contains("FSTART_FIXTURE_IMAGE=recorded\n") && !output.contains("AUX_INPUT="));
}

/// Profile rustflags the unit applies to this workspace's own crates.
pub(crate) fn member_flags(selection: &Selection) -> Vec<String> {
    let package = &selection
        .args
        .windows(2)
        .find(|pair| pair[0] == "--package")
        .unwrap()[1];
    selection.package_flags[package].clone()
}
