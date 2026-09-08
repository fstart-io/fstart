//! One compiler invocation for resolved builds, checks and editor checks.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use crate::{board_manifest::BoardManifest, resolved::ResolvedBuild};

pub(crate) struct Selection {
    pub directory: PathBuf,
    pub manifest: PathBuf,
    pub cfgs: Vec<String>,
    pub flags: Vec<String>,
    pub args: Vec<String>,
}

impl Selection {
    pub fn prepare(
        root: &Path,
        board: &BoardManifest,
        layout: &ResolvedBuild,
        release: bool,
    ) -> Result<Self, String> {
        if std::env::var_os("FSTART_EXTRA_RUSTFLAGS").is_some() {
            return Err("resolved builds do not accept unrecorded FSTART_EXTRA_RUSTFLAGS".into());
        }
        let directory = layout.artifact_dir(root, &board.board, release)?;
        fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
        let script = directory.join("link.ld");
        fs::write(&script, crate::linker::resolved_xip(layout)?).map_err(|e| e.to_string())?;
        fs::write(directory.join("resolved-build.json"), layout.json()?)
            .map_err(|e| e.to_string())?;
        let manifest = root
            .join("target/fstart-workspaces")
            .join(&board.board)
            .join("Cargo.toml");
        let payload = if layout.payload == "uefi" {
            "crabefi"
        } else {
            &layout.payload
        };
        let cfgs = vec![
            format!("fstart_stage_env=\"{}\"", layout.env),
            format!("fstart_entry=\"{}\"", layout.entry),
            format!("fstart_payload=\"{payload}\""),
        ];
        let mut flags: Vec<String> = crate::toolchain::rustflags_for_triple(&layout.target)
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        for cfg in &cfgs {
            flags.extend(["--cfg".into(), cfg.clone()]);
        }
        flags.extend([
            "--check-cfg=cfg(fstart_stage_env,values(\"monolithic\"))".into(),
            "--check-cfg=cfg(fstart_entry,values(\"riscv64\",\"armv7\"))".into(),
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
            layout.target.clone(),
            "--target-dir".into(),
            directory.join("cargo").display().to_string(),
            "--features".into(),
            layout.features.join(","),
            "-Z".into(),
            "build-std=core,alloc".into(),
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
        })
    }

    pub fn environment(&self) -> BTreeMap<String, String> {
        // Encoded flags also preserve workspace paths containing whitespace.
        BTreeMap::from([
            ("CARGO_ENCODED_RUSTFLAGS".into(), self.flags.join("\u{1f}")),
            ("RUSTFLAGS".into(), String::new()),
        ])
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
