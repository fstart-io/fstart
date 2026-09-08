//! One compiler invocation for resolved builds, checks and editor checks.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use crate::{board_manifest::BoardManifest, resolved::ResolvedBuild};

#[cfg(test)]
#[path = "selection_tests.rs"]
mod tests;

/// A single compiler unit projected by a family resolver. Geometry and stage
/// ordering stay with that resolver; build, check and IDE consume this same unit.
#[derive(serde::Serialize)]
pub(crate) struct StageInput<'a> {
    pub target: &'a str,
    pub entry: &'a str,
    pub env: &'a str,
    pub payload: &'a str,
    pub features: &'a [String],
    pub build_std: &'a str,
    pub directory: PathBuf,
    pub linker_script: String,
    pub resolved_json: String,
    pub producer_environment: BTreeMap<String, String>,
}

pub(crate) struct Selection {
    pub directory: PathBuf,
    pub manifest: PathBuf,
    pub cfgs: Vec<String>,
    pub flags: Vec<String>,
    pub args: Vec<String>,
    producer_environment: BTreeMap<String, String>,
}

impl Selection {
    pub fn prepare(
        root: &Path,
        board: &BoardManifest,
        layout: &ResolvedBuild,
        release: bool,
    ) -> Result<Self, String> {
        Self::prepare_stage(
            root,
            board,
            StageInput {
                target: &layout.target,
                entry: &layout.entry,
                env: &layout.env,
                payload: &layout.payload,
                features: &layout.features,
                build_std: "core,alloc",
                directory: layout.artifact_dir(root, &board.board, release)?,
                linker_script: crate::linker::resolved_xip(layout)?,
                resolved_json: layout.json()?,
                producer_environment: BTreeMap::new(),
            },
            release,
        )
    }

    pub fn prepare_stage(
        root: &Path,
        board: &BoardManifest,
        input: StageInput<'_>,
        release: bool,
    ) -> Result<Self, String> {
        if std::env::var_os("FSTART_EXTRA_RUSTFLAGS").is_some() {
            return Err("resolved builds do not accept unrecorded FSTART_EXTRA_RUSTFLAGS".into());
        }
        // A shell's ramstage producer outputs must not silently leak into a
        // different stage (including IDE producer-unit preparation).
        for name in ["FSTART_SMM_IMAGE", "FSTART_SMM_COREBOOT_HEADER"] {
            if std::env::var_os(name).is_some() && !input.producer_environment.contains_key(name) {
                return Err(format!(
                    "unrecorded producer input {name}; clear it before selecting this stage"
                ));
            }
        }
        if input
            .producer_environment
            .keys()
            .any(|name| matches!(name.as_str(), "RUSTFLAGS" | "CARGO_ENCODED_RUSTFLAGS"))
        {
            return Err("producer inputs cannot override compiler flags".into());
        }
        fs::create_dir_all(&input.directory).map_err(|e| e.to_string())?;
        fs::write(
            input.directory.join("compiler-selection.json"),
            serde_json::to_vec_pretty(&input).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let directory = input.directory;
        let script = directory.join("link.ld");
        fs::write(&script, &input.linker_script).map_err(|e| e.to_string())?;
        fs::write(directory.join("resolved-build.json"), &input.resolved_json)
            .map_err(|e| e.to_string())?;
        let manifest = root
            .join("target/fstart-workspaces")
            .join(&board.board)
            .join("Cargo.toml");
        let payload = if input.payload == "uefi" {
            "crabefi"
        } else {
            input.payload
        };
        let cfgs = vec![
            format!("fstart_stage_env=\"{}\"", input.env),
            format!("fstart_entry=\"{}\"", input.entry),
            format!("fstart_payload=\"{payload}\""),
        ];
        let mut flags: Vec<String> = crate::toolchain::rustflags_for_triple(input.target)
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        for cfg in &cfgs {
            flags.extend(["--cfg".into(), cfg.clone()]);
        }
        flags.extend([
            crate::toolchain::STAGE_ENV_CHECK_CFG.into(),
            "--check-cfg=cfg(fstart_entry,values(\"riscv64\",\"armv7\",\"aarch64-relocate\",\"x86_64\"))"
                .into(),
            "--check-cfg=cfg(fstart_payload,values(\"halt\",\"linux\",\"crabefi\"))".into(),
            format!("-Clink-arg=-T{}", script.display()),
        ]);
        fs::write(directory.join("rustflags.txt"), flags.join(" ")).map_err(|e| e.to_string())?;
        fs::write(
            directory.join("rustflags.json"),
            serde_json::to_vec_pretty(&flags).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let mut args = vec![
            "--locked".into(),
            "--no-default-features".into(),
            "--manifest-path".into(),
            manifest.display().to_string(),
            "--package".into(),
            board.package.clone(),
            "--bin".into(),
            board.stage_bin.clone().ok_or("missing stage-bin")?,
            "--target".into(),
            input.target.to_owned(),
            "--target-dir".into(),
            directory.join("cargo").display().to_string(),
            "--features".into(),
            input.features.join(","),
            "-Z".into(),
            format!("build-std={}", input.build_std),
        ];
        if release {
            args.push("--release".into());
        }
        Ok(Self {
            directory,
            manifest,
            cfgs,
            flags,
            args,
            producer_environment: input.producer_environment,
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
            vec![
                "cargo".into(),
                "check".into(),
                "--message-format=json".into(),
            ],
            self.args.clone(),
        ]
        .concat()
    }

    pub fn command(&self, build: bool) -> Command {
        let mut command = Command::new("cargo");
        command
            .arg(if build { "build" } else { "check" })
            .arg("--message-format=json-render-diagnostics")
            .args(&self.args)
            .envs(self.environment());
        command
    }
}
