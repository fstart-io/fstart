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
