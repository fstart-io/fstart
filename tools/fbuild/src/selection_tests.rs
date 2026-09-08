use super::{Selection, StageInput};
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
fn stage_selections_keep_cfgs_features_and_producer_inputs_separate() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let board = crate::board_manifest::find(root, "lenovo-x61").unwrap();
    let scratch = Scratch(
        std::env::temp_dir().join(format!("fstart stage selection {}", std::process::id())),
    );
    let features = vec!["stage".to_owned(), "fstart-stage/x86_64".to_owned()];
    for (env, payload) in [("car", "halt"), ("postcar", "halt"), ("ram", "uefi")] {
        let producer_environment = if env == "ram" {
            BTreeMap::from([(
                "FSTART_SMM_IMAGE".to_owned(),
                scratch.0.join("SMM image.bin").display().to_string(),
            )])
        } else {
            BTreeMap::new()
        };
        let directory = scratch.0.join(env);
        let selection = Selection::prepare_stage(
            root,
            &board,
            StageInput {
                target: "x86_64-unknown-none",
                entry: "x86_64",
                env,
                payload,
                features: &features,
                build_std: "core,alloc",
                directory: directory.clone(),
                linker_script: "/* synthetic selection, not linked */".into(),
                resolved_json: "{}".into(),
                producer_environment: producer_environment.clone(),
            },
            true,
        )
        .unwrap();
        assert!(
            selection
                .cfgs
                .contains(&format!("fstart_stage_env=\"{env}\""))
        );
        assert!(selection.cfgs.contains(&format!(
            "fstart_payload=\"{}\"",
            if env == "ram" { "crabefi" } else { "halt" }
        )));
        assert!(
            selection
                .args
                .windows(2)
                .any(|args| args == ["--features", &features.join(",")])
        );
        let actual_env = selection.environment();
        assert_eq!(
            actual_env.get("FSTART_SMM_IMAGE"),
            producer_environment.get("FSTART_SMM_IMAGE")
        );
        let encoded = &actual_env["CARGO_ENCODED_RUSTFLAGS"];
        assert!(encoded.split('\u{1f}').any(|flag| flag == format!("-Clink-arg=-T{}", directory.join("link.ld").display())));
        for build in [false, true] {
            let command = selection.command(build);
            let command_env: BTreeMap<_, _> = command
                .get_envs()
                .map(|(name, value)| {
                    (
                        name.to_string_lossy().into_owned(),
                        value.unwrap().to_string_lossy().into_owned(),
                    )
                })
                .collect();
            assert_eq!(command_env, actual_env);
        }
        let receipt: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.join("compiler-selection.json")).unwrap())
                .unwrap();
        assert_eq!(receipt["env"], env);
        assert_eq!(
            receipt["producer_environment"],
            serde_json::to_value(producer_environment).unwrap()
        );
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
    assert_eq!(
        &unbound.check_command()[..5],
        ["env", "-u", "FSTART_FIXTURE_IMAGE", "-u", "AUX_INPUT"]
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
