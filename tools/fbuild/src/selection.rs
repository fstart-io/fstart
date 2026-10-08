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
pub(crate) mod tests;

pub(crate) struct Selection {
    pub directory: PathBuf,
    pub manifest: PathBuf,
    pub cfgs: Vec<String>,
    /// RUSTFLAGS for every crate in the unit, core and dependencies included.
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
        // checkout and FSTART_QEMU_* steer the emulator launch, not firmware
        // content. Known unbound keys are removed below.
        for (key, _) in std::env::vars_os() {
            let name = key.to_string_lossy();
            if name.starts_with("FSTART_")
                && name != "FSTART_WORKSPACE_ROOT"
                && !name.starts_with("FSTART_QEMU_")
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
        let flags = unit.rustflags.clone();
        // The stage selection and linker script only concern this workspace's
        // crates. Passing them as profile rustflags rather than RUSTFLAGS keeps
        // core, alloc and registry dependencies identical between units, so
        // every unit shares one Cargo target directory and builds those once.
        let mut member_flags: Vec<String> = cfgs
            .iter()
            .flat_map(|cfg| ["--cfg".into(), cfg.clone()])
            .collect();
        member_flags.extend(unit.check_cfg_flags()?);
        if let Some(script) = &unit.linker_script {
            let path = directory.join("link.ld");
            fs::write(&path, script).map_err(|e| e.to_string())?;
            member_flags.push(format!("-Clink-arg=-T{}", path.display()));
        }
        fs::write(
            directory.join("rustflags.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "rustflags": flags,
                "member_rustflags": member_flags,
            }))
            .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let profile = if release || unit.release_only {
            "release"
        } else {
            "dev"
        };
        // `"*"` matches every non-member package; build-override covers
        // build scripts and proc macros, which run on the host.
        let member_rustflags = directory.join("member-rustflags.toml");
        fs::write(
            &member_rustflags,
            format!(
                "[profile.{profile}]\nrustflags = {}\n\
                 [profile.{profile}.package.\"*\"]\nrustflags = []\n\
                 [profile.{profile}.build-override]\nrustflags = []\n",
                serde_json::to_string(&member_flags).map_err(|e| e.to_string())?
            ),
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
            shared_target_dir(root).display().to_string(),
            "-Zprofile-rustflags".into(),
            "--config".into(),
            member_rustflags.display().to_string(),
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

    /// Cargo target directory holding this unit's compiler outputs.
    pub fn target_dir(&self, root: &Path) -> PathBuf {
        shared_target_dir(root)
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

/// One Cargo target directory for all firmware units of all boards. Cargo
/// keys every artifact by its full configuration, so units only share what
/// is really identical, such as core for one target.
fn shared_target_dir(root: &Path) -> PathBuf {
    root.join("target/fstart-build/cargo")
}
