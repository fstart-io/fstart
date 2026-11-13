//! Evaluate a board's Rust plan on the host using the source workspace's pins.
//! Cargo prepares the disposable runner's lock offline from a copy of Cargo.lock.
//! Every external pin must already exist in that source lock; the build then
//! runs --locked. No manifest or lock in the source workspace is rewritten.
use crate::profile_source::ProfileSource;
use fstart_image_build::plan::{BuildSelection, PlanEnvelope, ResolvedPlan};
use std::{fs, path::Path, process::Command};

const RUNNER: &str = "fstart-plan-runner";

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

pub(crate) fn load(
    root: &Path,
    source: &ProfileSource,
    board: &crate::board_manifest::BoardManifest,
    selection: &BuildSelection,
) -> Result<ResolvedPlan, String> {
    let runner = root
        .join("target/fstart-host")
        .join(&board.board)
        .join("plan-runner");
    fs::create_dir_all(runner.join("src")).map_err(|e| e.to_string())?;
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
    // The runner names the `HostPlan` trait, so the plan contract crate
    // is a direct dependency of the generated manifest too.
    let image_build_path = root.join("crates/image-build").display().to_string();
    let manifest = format!(
        "[workspace]\n[package]\nname = \"{RUNNER}\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[dependencies]\nboard = {{ package = {}, path = {}, default-features = false, features = {} }}\nplatform = {{ package = {}, path = {}, default-features = false, features = [\"host\"] }}\nfstart-image-build = {{ path = {} }}\n",
        quoted(&board_name),
        quoted(&board_path),
        serde_json::to_string(&board.variant_features).map_err(|e| e.to_string())?,
        quoted(&platform_name),
        quoted(&platform_path),
        quoted(&image_build_path)
    );
    let module = board
        .build_profile
        .as_ref()
        .and_then(|profile| profile.module.as_deref())
        .map(|module| format!("{module}::"))
        .unwrap_or_default();
    let main = format!(
        "fn main() {{ use fstart_image_build::plan::HostPlan as _; platform::{module}Plan::<board::{}>::emit(&std::env::args().nth(1).expect(\"missing build selection\")); }}\n",
        board.board_type
    );
    for (path, contents) in [
        (runner.join("Cargo.toml"), manifest.as_str()),
        (runner.join("src/main.rs"), main.as_str()),
    ] {
        if fs::read_to_string(&path).ok().as_deref() != Some(contents) {
            fs::write(path, contents).map_err(|e| e.to_string())?;
        }
    }
    let manifest_path = runner.join("Cargo.toml");
    let lock_path = runner.join("Cargo.lock");
    let locked = prepare_lock(root, &manifest_path)?;
    let rustc = Command::new("rustc")
        .current_dir(root)
        .arg("-vV")
        .output()
        .map_err(|e| e.to_string())?;
    if !rustc.status.success() {
        return Err("cannot query selected Rust host toolchain".into());
    }
    let rustc = String::from_utf8(rustc.stdout).map_err(|e| e.to_string())?;
    let host = rustc
        .lines()
        .find_map(|s| s.strip_prefix("host: "))
        .ok_or("rustc did not report host target")?;
    let target = root.join("target/fstart-host").join(&board.board);
    let output = Command::new("cargo")
        .current_dir(root)
        .args(["build", "--locked", "--manifest-path"])
        .arg(&manifest_path)
        .args(["-p", RUNNER, "--target", host, "--target-dir"])
        .arg(target)
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
    let expected_manifest = runner
        .join("Cargo.toml")
        .canonicalize()
        .map_err(|e| e.to_string())?;
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
            executables.push(file.to_owned());
        }
    }
    if executables.len() != 1 {
        return Err("Cargo did not report exactly one current host runner executable".into());
    }
    // The platform reads its own and the board's firmware tables.
    let mut selection = selection.clone();
    selection.manifests = fstart_image_build::plan::PackageTables {
        board: source.package["metadata"]["fstart"].clone(),
        platform: source.platform["metadata"]["fstart"].clone(),
    };
    let output = Command::new(&executables[0])
        .arg(serde_json::to_string(&selection).map_err(|e| e.to_string())?)
        .current_dir(root)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "platform plan failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let envelope: PlanEnvelope = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("platform plan transport: {e}"))?;
    envelope
        .into_plan()
        .map_err(|e| format!("platform plan failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

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
