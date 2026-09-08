//! One compiler invocation for resolved builds, checks and editor checks.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use crate::board_manifest::BoardManifest;

#[cfg(test)]
#[path = "selection_tests.rs"]
mod tests;

pub(crate) struct Selection {
    pub directory: PathBuf,
    pub manifest: PathBuf,
    pub cfgs: Vec<String>,
    pub flags: Vec<String>,
    pub args: Vec<String>,
    producer_environment: BTreeMap<String, String>,
    removed_environment: Vec<String>,
}

impl Selection {
    /// Concrete compiler unit shared by the common build/check/editor executor.
    pub fn prepare_unit(
        root: &Path,
        board: &BoardManifest,
        unit: &fstart_image_build::build_plan::CompilationUnit,
        directory: PathBuf,
        resolved_json: &str,
        environment: BTreeMap<String, String>,
        artifact_environment: &[String],
        release: bool,
    ) -> Result<Self, String> {
        use fstart_image_build::build_plan::CargoTarget;
        if std::env::var_os("FSTART_EXTRA_RUSTFLAGS").is_some() {
            return Err("resolved builds do not accept unrecorded FSTART_EXTRA_RUSTFLAGS".into());
        }
        if environment
            .keys()
            .any(|key| matches!(key.as_str(), "RUSTFLAGS" | "CARGO_ENCODED_RUSTFLAGS"))
        {
            return Err("artifact bindings cannot override compiler flags".into());
        }
        // Fail closed on undeclared ambient firmware inputs, including producer
        // namespaces belonging to another plan. WORKSPACE_ROOT selects this tool's
        // checkout, not firmware content. Known unbound keys are removed below.
        for (key, _) in std::env::vars_os() {
            let name = key.to_string_lossy();
            if name.starts_with("FSTART_")
                && name != "FSTART_WORKSPACE_ROOT"
                && !artifact_environment.iter().any(|k| k == &name)
                && !environment.contains_key(name.as_ref())
            {
                return Err(format!("unrecorded ambient firmware input {name}"));
            }
        }
        let removed_environment = artifact_environment
            .iter()
            .filter(|key| !environment.contains_key(*key))
            .cloned()
            .collect();
        fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
        fs::write(directory.join("resolved-build.json"), resolved_json)
            .map_err(|e| e.to_string())?;
        fs::write(
            directory.join("compiler-selection.json"),
            serde_json::to_vec_pretty(
                &serde_json::json!({"unit": unit, "environment": environment, "removed_environment": removed_environment}),
            )
            .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let cfgs = vec![
            format!("fstart_stage_env=\"{}\"", unit.environment),
            format!("fstart_entry=\"{}\"", unit.entry),
            format!("fstart_payload=\"{}\"", unit.payload),
        ];
        let mut flags = unit.rustflags.clone();
        for cfg in &cfgs {
            flags.extend(["--cfg".into(), cfg.clone()]);
        }
        flags.extend(unit.check_cfg_flags()?);
        if let Some(script) = &unit.linker_script {
            let path = directory.join("link.ld");
            fs::write(&path, script).map_err(|e| e.to_string())?;
            flags.push(format!("-Clink-arg=-T{}", path.display()));
        }
        fs::write(
            directory.join("rustflags.json"),
            serde_json::to_vec_pretty(&flags).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let manifest = root
            .join("target/fstart-workspaces")
            .join(&board.board)
            .join("Cargo.toml");
        let mut args = vec![
            "--locked".into(),
            "--no-default-features".into(),
            "--manifest-path".into(),
            manifest.display().to_string(),
            "--package".into(),
            board.package.clone(),
        ];
        match &unit.cargo_target {
            CargoTarget::BoardBinary => args.extend([
                "--bin".into(),
                board.stage_bin.clone().ok_or("missing stage-bin")?,
            ]),
            CargoTarget::BoardLibrary => args.push("--lib".into()),
        }
        args.extend([
            "--target".into(),
            unit.target.clone(),
            "--target-dir".into(),
            directory.join("cargo").display().to_string(),
            "--features".into(),
            unit.features.join(","),
        ]);
        if let Some(std) = &unit.build_std {
            args.extend(["-Z".into(), format!("build-std={std}")]);
        }
        if release || unit.release_only {
            args.push("--release".into());
        }
        Ok(Self {
            directory,
            manifest,
            cfgs,
            flags,
            args,
            producer_environment: environment,
            removed_environment,
        })
    }

    pub fn environment(&self) -> BTreeMap<String, String> {
        // Encoded flags also preserve workspace paths containing whitespace.
        self.producer_environment
            .clone()
            .into_iter()
            .chain([
                ("CARGO_ENCODED_RUSTFLAGS".into(), self.flags.join("\u{1f}")),
                ("RUSTFLAGS".into(), String::new()),
            ])
            .collect()
    }

    pub fn check_command(&self) -> Vec<String> {
        [
            if self.removed_environment.is_empty() {
                vec![]
            } else {
                std::iter::once("env".into())
                    .chain(
                        self.removed_environment
                            .iter()
                            .flat_map(|key| ["-u".into(), key.clone()]),
                    )
                    .collect()
            },
            vec![
                "cargo".into(),
                "check".into(),
                "--message-format=json".into(),
            ],
            self.args.clone(),
        ]
        .concat()
    }

    pub fn apply_environment(&self, command: &mut Command) {
        for key in &self.removed_environment {
            command.env_remove(key);
        }
        command.envs(self.environment());
    }

    pub fn command(&self, build: bool) -> Command {
        let mut command = Command::new("cargo");
        command
            .arg(if build { "build" } else { "check" })
            .arg("--message-format=json-render-diagnostics")
            .args(&self.args);
        self.apply_environment(&mut command);
        command
    }
}
