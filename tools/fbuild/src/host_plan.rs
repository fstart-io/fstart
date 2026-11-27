//! Evaluate a board's Rust plan on the host using the source workspace's pins.
//! Cargo prepares the disposable runner's lock offline from a copy of Cargo.lock.
//! Every external pin must already exist in that source lock; the build then
//! runs --locked. No manifest or lock in the source workspace is rewritten.
use crate::profile_source::ProfileSource;
use fstart_image_build::plan::{BuildSelection, PlanEnvelope, PlanError, ResolvedPlan};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{Mutex, Once, OnceLock},
};

const RUNNER: &str = "fstart-plan-runner";

const RUNNER_MAIN: &str = "fn main() { use fstart_image_build::plan::HostPlan as _; platform::Plan::<board::Board>::emit(&std::env::args().nth(1).expect(\"missing build selection\")); }\n";

#[derive(Debug, serde::Deserialize, PartialEq, Eq, PartialOrd, Ord)]
struct Pin {
    name: String,
    version: String,
    source: Option<String>,
    checksum: Option<String>,
}

fn external_pins(text: &str) -> Result<std::collections::BTreeSet<Pin>, String> {
    #[derive(serde::Deserialize)]
    struct Lock {
        package: Vec<Pin>,
    }
    let lock: Lock = toml::from_str(text).map_err(|e| format!("Cargo.lock: {e}"))?;
    Ok(lock
        .package
        .into_iter()
        .filter(|pin| pin.source.is_some())
        .collect())
}

fn prepare_lock(root: &Path, manifest: &Path) -> Result<String, String> {
    let before = fs::read_to_string(root.join("Cargo.lock")).map_err(|e| e.to_string())?;
    let lock_path = manifest.with_file_name("Cargo.lock");
    fs::write(&lock_path, &before).map_err(|e| e.to_string())?;
    let prepared = Command::new("cargo")
        .current_dir(root)
        .args([
            "metadata",
            "--offline",
            "--format-version=1",
            "--manifest-path",
        ])
        .arg(manifest)
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env("RUSTFLAGS", "")
        .output()
        .map_err(|e| e.to_string())?;
    if !prepared.status.success() {
        return Err(format!(
            "host plan lock preparation: {}",
            String::from_utf8_lossy(&prepared.stderr)
        ));
    }
    let locked = fs::read_to_string(&lock_path).map_err(|e| e.to_string())?;
    if !external_pins(&locked)?.is_subset(&external_pins(&before)?) {
        return Err("host plan dependencies differ from the source Cargo.lock".into());
    }
    Ok(locked)
}

/// The source workspace's `[patch]` tables, for the runner's manifest: the
/// runner must resolve a patched dependency as the workspace does. A
/// patch's `path` is made absolute; its other keys are strings.
fn patches(root: &Path) -> Result<String, String> {
    let text = fs::read_to_string(root.join("Cargo.toml")).map_err(|e| e.to_string())?;
    let manifest: toml::Table = toml::from_str(&text).map_err(|e| format!("Cargo.toml: {e}"))?;
    let mut out = String::new();
    let Some(patch) = manifest.get("patch").and_then(toml::Value::as_table) else {
        return Ok(out);
    };
    let quoted = |s: &str| serde_json::to_string(s).unwrap();
    for (source, crates) in patch {
        out.push_str(&format!("[patch.{}]\n", quoted(source)));
        let crates = crates
            .as_table()
            .ok_or("Cargo.toml: [patch] is not a table")?;
        for (name, spec) in crates {
            let spec = spec
                .as_table()
                .ok_or("Cargo.toml: a patch is not a table")?;
            let mut keys = Vec::new();
            for (key, value) in spec {
                let value = value
                    .as_str()
                    .ok_or_else(|| format!("Cargo.toml: patch {name}.{key} is not a string"))?;
                let value = if key == "path" {
                    root.join(value).display().to_string()
                } else {
                    value.into()
                };
                keys.push(format!("{key} = {}", quoted(&value)));
            }
            out.push_str(&format!("{} = {{ {} }}\n", quoted(name), keys.join(", ")));
        }
    }
    Ok(out)
}

fn built_runners() -> &'static Mutex<BTreeMap<(PathBuf, String), PathBuf>> {
    static CACHE: OnceLock<Mutex<BTreeMap<(PathBuf, String), PathBuf>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// The name of a runner's directory, package and binary. Cargo.lock is not
/// part of it: every build re-prepares the runner's lock from the source
/// lock, so a lock change rebuilds the runner in place rather than adding
/// another directory and binary to `target/fstart-host`.
fn runner_key(manifest_body: &str, main_src: &str) -> String {
    fn field(hash: &mut Sha256, bytes: &[u8]) {
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    }
    let mut hash = Sha256::new();
    for s in [manifest_body, main_src] {
        field(&mut hash, s.as_bytes());
    }
    crate::hex_digest(hash.finalize())[..16].into()
}

/// Remove the per-board Cargo target directories of the old runner layout,
/// `<host_dir>/<board>/plan-runner`, which the shared `cargo` target
/// directory replaced. Each one holds a full host dependency build.
fn prune_legacy_runners(host_dir: &Path) {
    let Ok(entries) = fs::read_dir(host_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if entry.file_name() == "runners" || entry.file_name() == "cargo" {
            continue;
        }
        if dir.join("plan-runner/Cargo.toml").is_file() {
            match fs::remove_dir_all(&dir) {
                Ok(()) => eprintln!("[fstart] removed old host runner {}", dir.display()),
                Err(e) => eprintln!(
                    "[fstart] could not remove old host runner {}: {e}",
                    dir.display()
                ),
            }
        }
    }
}

fn build_isolated_runner(
    root: &Path,
    runner_dir: &Path,
    target_dir: &Path,
    runner_name: &str,
    manifest: &str,
    main_src: &str,
    host: &str,
) -> Result<PathBuf, String> {
    fs::create_dir_all(runner_dir.join("src")).map_err(|e| e.to_string())?;
    let lock_file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(runner_dir.join(".lock"))
        .map_err(|e| format!("runner lock: {e}"))?;
    lock_file.lock().map_err(|e| format!("runner lock: {e}"))?;

    for (path, contents) in [
        (runner_dir.join("Cargo.toml"), manifest),
        (runner_dir.join("src/main.rs"), main_src),
    ] {
        if fs::read_to_string(&path).ok().as_deref() != Some(contents) {
            fs::write(path, contents).map_err(|e| e.to_string())?;
        }
    }
    let manifest_path = runner_dir.join("Cargo.toml");
    let lock_path = runner_dir.join("Cargo.lock");
    let locked = prepare_lock(root, &manifest_path)?;
    let target_lock = crate::target_dir_lock(target_dir);
    let _target_guard = target_lock.lock().unwrap_or_else(|e| e.into_inner());
    let output = Command::new("cargo")
        .current_dir(root)
        .args(["build", "--locked", "--manifest-path"])
        .arg(&manifest_path)
        .args(["-p", runner_name, "--target", host, "--target-dir"])
        .arg(target_dir)
        .args(["--message-format=json-render-diagnostics"])
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        // The host adapter builds without ambient compiler flags.
        .env("RUSTFLAGS", "")
        .stderr(std::process::Stdio::inherit())
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err("platform host adapter failed with --locked; prepared lock was not re-resolved (no fallback)".into());
    }
    if fs::read_to_string(&lock_path).map_err(|e| e.to_string())? != locked {
        return Err("host adapter changed its prepared lock".into());
    }
    let expected_manifest = manifest_path.canonicalize().map_err(|e| e.to_string())?;
    let mut executables = Vec::new();
    for line in output
        .stdout
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
    {
        let row: serde_json::Value = serde_json::from_slice(line).map_err(|e| e.to_string())?;
        if row["reason"] == "compiler-artifact"
            && row["manifest_path"]
                .as_str()
                .and_then(|s| Path::new(s).canonicalize().ok())
                .as_ref()
                == Some(&expected_manifest)
            && let Some(file) = row["executable"].as_str()
        {
            executables.push(PathBuf::from(file));
        }
    }
    if executables.len() != 1 {
        return Err("Cargo did not report exactly one current host runner executable".into());
    }
    Ok(executables.remove(0))
}

fn build_runner(root: &Path, manifest_body: &str, main_src: &str) -> Result<PathBuf, String> {
    let key = runner_key(manifest_body, main_src);
    let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let cache_key = (canonical_root, key.clone());
    if let Some(cached) = built_runners()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&cache_key)
        .cloned()
        .filter(|path| path.is_file())
    {
        return Ok(cached);
    }
    let runner_name = format!("{RUNNER}-{key}");
    let manifest = format!(
        "[workspace]\n[package]\nname = \"{runner_name}\"\nversion = \"0.0.0\"\nedition = \"2024\"\n{manifest_body}"
    );
    let host_dir = root.join("target/fstart-host");
    static PRUNE: Once = Once::new();
    PRUNE.call_once(|| prune_legacy_runners(&host_dir));
    let built = build_isolated_runner(
        root,
        &host_dir.join("runners").join(&key),
        &host_dir.join("cargo"),
        &runner_name,
        &manifest,
        main_src,
        &crate::build_board::host_target(root)?,
    )?;
    built_runners()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(cache_key, built.clone());
    Ok(built)
}

const ACPI_RUNNER_MAIN: &str = "fn main() { use fstart_image_build::acpi::HostAcpi as _; let base = std::env::args().nth(1).expect(\"missing ACPI base\").parse().expect(\"invalid ACPI base\"); platform::Plan::<board::Board>::emit_acpi(base); }\n";

fn runner_manifest(
    root: &Path,
    source: &ProfileSource,
    board_features: &[String],
) -> Result<String, String> {
    let package = |p: &serde_json::Value| -> Result<(String, String), String> {
        Ok((
            p["name"].as_str().ok_or("missing package name")?.into(),
            Path::new(p["manifest_path"].as_str().ok_or("missing package path")?)
                .parent()
                .ok_or("missing package directory")?
                .display()
                .to_string(),
        ))
    };
    let (board_name, board_path) = package(&source.package)?;
    let (platform_name, platform_path) = package(&source.platform)?;
    let quoted = |s: &str| serde_json::to_string(s).unwrap();
    // The runner names `HostPlan` / `HostAcpi`, so the contract crate is a
    // direct dependency of the generated manifest too.
    let image_build_path = root.join("crates/image-build").display().to_string();
    let workspace_patches = patches(root)?;
    Ok(format!(
        "[dependencies]\nboard = {{ package = {}, path = {}, default-features = false, features = {} }}\nplatform = {{ package = {}, path = {}, default-features = false, features = [\"host\"] }}\nfstart-image-build = {{ path = {} }}\n{}",
        quoted(&board_name),
        quoted(&board_path),
        serde_json::to_string(board_features).map_err(|e| e.to_string())?,
        quoted(&platform_name),
        quoted(&platform_path),
        quoted(&image_build_path),
        workspace_patches,
    ))
}

pub(crate) fn load(
    root: &Path,
    source: &ProfileSource,
    board: &crate::board_manifest::BoardManifest,
    selection: &BuildSelection,
) -> Result<ResolvedPlan, PlanError> {
    let manifest_body = runner_manifest(root, source, &board.variant_features)?;
    let executable = build_runner(root, &manifest_body, RUNNER_MAIN)?;

    // The platform reads its own and the board's firmware tables.
    let mut selection = selection.clone();
    selection.manifests = fstart_image_build::plan::PackageTables {
        board: source.package["metadata"]["fstart"].clone(),
        platform: source.platform["metadata"]["fstart"].clone(),
    };
    let output = Command::new(&executable)
        .arg(serde_json::to_string(&selection).map_err(|e| e.to_string())?)
        .current_dir(root)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "platform plan failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    let envelope: PlanEnvelope = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("platform plan transport: {e}"))?;
    envelope.into_plan().map_err(|e| match e {
        PlanError::UnsupportedPayload(msg) => PlanError::UnsupportedPayload(msg),
        PlanError::Failed(msg) => PlanError::Failed(format!("platform plan failed: {msg}")),
    })
}

pub(crate) fn acpi_baselines(
    root: &Path,
    board: &crate::board_manifest::BoardManifest,
    base: u64,
) -> Result<fstart_image_build::acpi::AcpiOutcome, String> {
    let source = crate::profile_source::load_profile(root, board)?;
    let mut features = board.variant_features.clone();
    if source.package["features"].get("acpi").is_some()
        && !features.iter().any(|feature| feature == "acpi")
    {
        features.push("acpi".into());
    }
    let manifest_body = runner_manifest(root, &source, &features)?;
    let runner = build_runner(root, &manifest_body, ACPI_RUNNER_MAIN)?;
    let output = Command::new(runner)
        .arg(base.to_string())
        .current_dir(root)
        .output()
        .map_err(|e| format!("ACPI runner: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "ACPI runner: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let envelope: fstart_image_build::acpi::AcpiEnvelope =
        serde_json::from_slice(&output.stdout)
            .map_err(|e| format!("ACPI runner transport: {e}"))?;
    envelope.into_outcome()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_runner_takes_the_workspace_patches() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let text = fs::read_to_string(root.join("Cargo.toml")).unwrap();
        let patches = patches(&root).unwrap();
        assert_eq!(text.contains("[patch."), !patches.is_empty());
        let parsed: toml::Table = toml::from_str(&patches).unwrap();
        let workspace: toml::Table = toml::from_str(&text).unwrap();
        assert_eq!(parsed.get("patch"), workspace.get("patch"));
    }

    #[test]
    fn runner_uses_real_sources_without_rewriting_the_workspace_lock() {
        struct Scratch(std::path::PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let directory =
            Scratch(std::env::temp_dir().join(format!("fstart-host-lock-{}", std::process::id())));
        fs::create_dir_all(directory.0.join("board/src")).unwrap();
        fs::write(
            directory.0.join("Cargo.toml"),
            "[workspace]\nresolver='3'\nmembers=['board']\n",
        )
        .unwrap();
        fs::write(
            directory.0.join("board/Cargo.toml"),
            "[package]\nname='board-real'\nversion='0.1.0'\nedition='2024'\n",
        )
        .unwrap();
        fs::write(directory.0.join("board/src/lib.rs"), "pub struct Marker;\n").unwrap();
        let status = Command::new("cargo")
            .current_dir(&directory.0)
            .args(["generate-lockfile", "--offline"])
            .status()
            .unwrap();
        assert!(status.success());
        let before = fs::read_to_string(directory.0.join("Cargo.lock")).unwrap();
        let runner = directory.0.join("runner");
        fs::create_dir_all(runner.join("src")).unwrap();
        let manifest = runner.join("Cargo.toml");
        fs::write(&manifest, "[workspace]\n[package]\nname='fstart-plan-runner'\nversion='0.0.0'\nedition='2024'\n[dependencies]\nboard={package='board-real',path='../board'}\n").unwrap();
        fs::write(
            runner.join("src/main.rs"),
            "fn main() { let _ = board::Marker; }\n",
        )
        .unwrap();
        let expected = prepare_lock(&directory.0, &manifest).unwrap();
        for _ in 0..2 {
            let output = Command::new("cargo")
                .current_dir(&directory.0)
                .args(["build", "--offline", "--locked", "--manifest-path"])
                .arg(&manifest)
                .env_remove("CARGO_ENCODED_RUSTFLAGS")
                .env("RUSTFLAGS", "")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                fs::read_to_string(directory.0.join("Cargo.lock")).unwrap(),
                before
            );
            assert_eq!(
                fs::read_to_string(runner.join("Cargo.lock")).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn isolated_runners_share_target_dir_without_executable_collision() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for (dir, pkg, marker) in [
            ("board-a", "board-a", "ALPHA"),
            ("board-b", "board-b", "BETA"),
        ] {
            fs::create_dir_all(root.join(dir).join("src")).unwrap();
            fs::write(
                root.join(dir).join("Cargo.toml"),
                format!("[package]\nname='{pkg}'\nversion='0.1.0'\nedition='2024'\n"),
            )
            .unwrap();
            fs::write(
                root.join(dir).join("src/lib.rs"),
                format!("pub const MARKER: &str = \"{marker}\";\n"),
            )
            .unwrap();
        }
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nresolver='3'\nmembers=['board-a', 'board-b']\n",
        )
        .unwrap();
        let status = Command::new("cargo")
            .current_dir(root)
            .args(["generate-lockfile", "--offline"])
            .status()
            .unwrap();
        assert!(status.success());

        let host = crate::build_board::host_target(root).unwrap();
        let target_dir = root.join("target/fstart-host/cargo");
        let main_src = "fn main() { print!(\"{}\", board::MARKER); }\n";

        let build = |pkg: &str| {
            let board_path = root.join(pkg).display().to_string();
            let manifest_body = format!(
                "[dependencies]\nboard = {{ package = \"{pkg}\", path = \"{board_path}\" }}\n"
            );
            let key = runner_key(&manifest_body, main_src);
            let name = format!("{RUNNER}-{key}");
            let manifest = format!(
                "[workspace]\n[package]\nname = \"{name}\"\nversion = \"0.0.0\"\nedition = \"2024\"\n{manifest_body}"
            );
            let runner_dir = root.join("target/fstart-host/runners").join(&key);
            build_isolated_runner(
                root,
                &runner_dir,
                &target_dir,
                &name,
                &manifest,
                main_src,
                &host,
            )
            .unwrap()
        };

        let exe_a = build("board-a");
        let exe_b = build("board-b");
        assert_ne!(exe_a, exe_b);

        // Building B into the same target-dir must not overwrite A's executable.
        let out_a = Command::new(&exe_a).output().unwrap();
        let out_b = Command::new(&exe_b).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out_a.stdout), "ALPHA");
        assert_eq!(String::from_utf8_lossy(&out_b.stdout), "BETA");
    }

    #[test]
    fn pruning_removes_only_old_per_board_runner_targets() {
        let tmp = tempfile::tempdir().unwrap();
        let host_dir = tmp.path();
        for file in [
            "qemu-riscv64/plan-runner/Cargo.toml",
            "qemu-riscv64/debug/big.rlib",
            "runners/0123456789abcdef/Cargo.toml",
            "cargo/debug/fstart-plan-runner-0123456789abcdef",
            "unrelated/Cargo.toml",
        ] {
            let path = host_dir.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "").unwrap();
        }
        prune_legacy_runners(host_dir);
        assert!(!host_dir.join("qemu-riscv64").exists());
        for kept in ["runners", "cargo", "unrelated"] {
            assert!(host_dir.join(kept).exists(), "{kept}");
        }
    }

    #[test]
    fn pin_comparison_rejects_changed_versions_sources_and_checksums() {
        let original = "[[package]]\nname='dep'\nversion='1.0.0'\nsource='registry+https://example.com'\nchecksum='abc'\n";
        let pins = external_pins(original).unwrap();
        let local = format!("{original}[[package]]\nname='board'\nversion='0.1.0'\n");
        assert_eq!(external_pins(&local).unwrap(), pins);
        for changed in [
            original.replace("1.0.0", "1.0.1"),
            original.replace("example.com", "other.com"),
            original.replace("abc", "def"),
        ] {
            assert!(!external_pins(&changed).unwrap().is_subset(&pins));
        }
    }
}
