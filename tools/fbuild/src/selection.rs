//! One compiler invocation for resolved builds, checks and editor checks.

use sha2::{Digest, Sha256};
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
    /// RUSTFLAGS for every crate in the unit, core and dependencies included.
    pub flags: Vec<String>,
    /// Additional `--remap-path-prefix` flags applied only to firmware builds
    /// (not `cargo check` / IDE flycheck, which need real host paths for
    /// diagnostics).
    build_remap_flags: Vec<String>,
    pub args: Vec<String>,
    producer_environment: BTreeMap<String, String>,
    removed_environment: Vec<String>,
}

impl Selection {
    /// Concrete compiler unit shared by the common build/check/editor executor.
    #[allow(clippy::too_many_arguments)]
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
        let flags = unit.rustflags.clone();
        let mut build_remap_flags = Vec::new();
        append_remap_flags(&mut build_remap_flags, root);
        // The linker script only concerns the board package being linked.
        // Scoping it to `[profile.<p>.package.<board>]` rather than
        // `[profile.<p>]` (which hits every workspace member, since
        // `package."*"` only overrides non-member dependencies) keeps
        // workspace drivers, platforms, core, alloc and registry crates
        // identical across stages and boards so Cargo and sccache compile
        // them once.
        let mut board_flags: Vec<String> = Vec::new();
        if let Some(script) = &unit.linker_script {
            let path = directory.join("link.ld");
            fs::write(&path, script).map_err(|e| e.to_string())?;
            board_flags.push(format!("-Clink-arg=-T{}", path.display()));
        }
        fs::write(
            directory.join("rustflags.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "rustflags": flags,
                "build_remap_rustflags": build_remap_flags,
                "board_rustflags": board_flags,
            }))
            .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let profile = if release || unit.release_only {
            "release"
        } else {
            "dev"
        };
        // Every firmware crate is size-optimized, in dev builds too:
        // opt-level 0 frames overflow the 8 KiB CAR/SRAM boot stacks.
        // Per-package overrides in Cargo.toml (crypto, lz4_flex) still win.
        // Only the board package gets the unit's linker-script rustflags.
        let profile_config = directory.join("profile.toml");
        let mut profile_toml = format!("[profile.{profile}]\nopt-level = \"s\"\n");
        if !board_flags.is_empty() {
            profile_toml.push_str(&format!(
                "[profile.{profile}.package.{}]\nrustflags = {}\n",
                serde_json::to_string(&board.package).map_err(|e| e.to_string())?,
                serde_json::to_string(&board_flags).map_err(|e| e.to_string())?,
            ));
        }
        fs::write(&profile_config, profile_toml).map_err(|e| e.to_string())?;
        let manifest = root.join("Cargo.toml");
        let mut args = vec![
            "--locked".into(),
            "--no-default-features".into(),
            "--manifest-path".into(),
            manifest.display().to_string(),
            "--package".into(),
            board.package.clone(),
        ];
        match &unit.cargo_target {
            // A board owns one binary per stage, named after the unit.
            CargoTarget::BoardBinary => args.extend(["--bin".into(), unit.name.clone()]),
            CargoTarget::BoardLibrary => args.push("--lib".into()),
        }
        // Targets rustc does not ship (32-bit bare-metal x86) are JSON specs
        // in `tools/fbuild/targets/`; Cargo still names their output directory by the stem.
        let target_spec = root
            .join("tools/fbuild/targets")
            .join(format!("{}.json", unit.target));
        let custom_target = target_spec.is_file();
        args.extend([
            "--target".into(),
            if custom_target {
                target_spec.display().to_string()
            } else {
                unit.target.clone()
            },
            "--target-dir".into(),
            shared_target_dir(root, &flags).display().to_string(),
            "-Zprofile-rustflags".into(),
            "--config".into(),
            profile_config.display().to_string(),
            "--features".into(),
            unit.features.join(","),
        ]);
        if let Some(std) = &unit.build_std {
            args.extend([
                "-Z".into(),
                format!("build-std={std}"),
                "-Z".into(),
                "build-std-features=optimize_for_size".into(),
            ]);
        }
        if custom_target {
            args.push("-Zjson-target-spec".into());
        }
        if release || unit.release_only {
            args.push("--release".into());
        }
        Ok(Self {
            directory,
            manifest,
            flags,
            build_remap_flags,
            args,
            producer_environment: environment,
            removed_environment,
        })
    }

    /// Cargo target directory holding this unit's compiler outputs.
    pub fn target_dir(&self, root: &Path) -> PathBuf {
        shared_target_dir(root, &self.flags)
    }

    pub fn environment(&self) -> BTreeMap<String, String> {
        self.environment_with_flags(&self.flags)
    }

    /// Environment for firmware builds, adding `--remap-path-prefix` so
    /// compiled stages do not embed checkout or toolchain paths.
    pub fn build_environment(&self) -> BTreeMap<String, String> {
        let mut all = self.flags.clone();
        all.extend(self.build_remap_flags.iter().cloned());
        self.environment_with_flags(&all)
    }

    fn environment_with_flags(&self, flags: &[String]) -> BTreeMap<String, String> {
        // Encoded flags also preserve workspace paths containing whitespace.
        self.producer_environment
            .clone()
            .into_iter()
            .chain([
                ("CARGO_ENCODED_RUSTFLAGS".into(), flags.join("\u{1f}")),
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
        for key in &self.removed_environment {
            command.env_remove(key);
        }
        if build {
            command.envs(self.build_environment());
        } else {
            command.envs(self.environment());
        }
        command
    }
}

/// Remap host-specific workspace, Cargo, Rustup, and sysroot prefixes so
/// firmware builds do not embed checkout paths, home directories, or host
/// triples in panic locations or debuginfo.
///
/// When several prefixes match a path, rustc applies the one given last, and
/// these nest: with rustup the sysroot lies inside `RUSTUP_HOME`, and
/// `CARGO_HOME` can lie inside the workspace. The flags are therefore ordered
/// from the shortest prefix to the longest, so the most specific one wins.
fn append_remap_flags(flags: &mut Vec<String>, root: &Path) {
    let mut prefixes: Vec<(String, &str)> = Vec::new();
    let mut push_path = |path: &Path, to: &'static str| {
        let canonical = path.canonicalize().ok();
        for path in [Some(path), canonical.as_deref()].into_iter().flatten() {
            if let Some(from) = path.to_str()
                && !prefixes.iter().any(|(known, _)| known == from)
            {
                prefixes.push((from.to_string(), to));
            }
        }
    };
    let resolve_env_path = |var: &str, home_sub: &str| -> Option<PathBuf> {
        let raw = std::env::var_os(var)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .filter(|v| !v.is_empty())
                    .map(|h| PathBuf::from(h).join(home_sub))
            })?;
        if raw.is_relative() {
            std::env::current_dir().ok().map(|cwd| cwd.join(&raw))
        } else {
            Some(raw)
        }
    };

    push_path(root, "/fstart");
    if let Some(path) = resolve_env_path("CARGO_HOME", ".cargo") {
        push_path(&path, "/cargo");
    }
    if let Some(path) = resolve_env_path("RUSTUP_HOME", ".rustup") {
        push_path(&path, "/rustup");
    }
    if let Some(sysroot) = rustc_sysroot() {
        // The stdlib sources get their own prefix, so the host triple in
        // `toolchains/<channel>-<host>/lib/rustlib/src/rust` is stripped too.
        push_path(&sysroot, "/rustc-sysroot");
        push_path(&sysroot.join("lib/rustlib/src/rust"), "/rustc");
    }
    // Stable: equal lengths keep the order above.
    prefixes.sort_by_key(|(from, _)| from.len());
    for (from, to) in prefixes {
        flags.push(format!("--remap-path-prefix={from}={to}"));
    }
}

fn rustc_sysroot() -> Option<PathBuf> {
    let rustc = std::env::var_os("RUSTC")
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "rustc".into());
    let output = Command::new(rustc)
        .args(["--print", "sysroot"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let trimmed = String::from_utf8(output.stdout).ok()?;
    let trimmed = trimmed.trim();
    (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
}

/// Cargo target directory keyed by the unit's global `RUSTFLAGS`.
///
/// Cargo does not include `CARGO_ENCODED_RUSTFLAGS` in a unit's metadata hash:
/// changing global flags (such as `-Crelocation-model=pic` for bootblock vs
/// `static` for postcar/ramstage vs `pie` for SMM) clobbers `core`, `alloc`,
/// registry crates, and workspace drivers in place. Partitioning the target
/// directory by `unit.rustflags` lets boards share all compiled dependencies
/// for each flag set without stages overwriting one another.
fn shared_target_dir(root: &Path, rustflags: &[String]) -> PathBuf {
    let mut hash = Sha256::new();
    for flag in rustflags {
        hash.update((flag.len() as u64).to_le_bytes());
        hash.update(flag.as_bytes());
    }
    let key = &crate::hex_digest(hash.finalize())[..16];
    root.join("target/fstart-build/cargo").join(key)
}
