//! Compile a thin conventional platform export in Cargo's selected-source graph.
//! The pre-existing preparation resolves a root-seeded disposable lock. Only
//! after that preparation do we append the runner's local entry and use --locked.
use crate::profile_source::ProfileSource;
use fstart_image_build::plan::{BuildSelection, ResolvedPlan};
use std::{fs, path::Path, process::Command};

const RUNNER: &str = "fstart-plan-runner";

/// Preserve every prepared package entry/pin verbatim. No resolver fallback.
fn runner_lock(lock: &str, dependencies: &[String]) -> Result<String, String> {
    let parsed: toml::Value = toml::from_str(lock).map_err(|e| e.to_string())?;
    let packages = parsed["package"].as_array().ok_or("lock has no packages")?;
    let mut dependencies = dependencies.to_vec();
    dependencies.sort();
    dependencies.dedup();
    for name in &dependencies {
        if packages
            .iter()
            .filter(|p| p["name"].as_str() == Some(name))
            .count()
            != 1
        {
            return Err(format!(
                "runner dependency {name} must identify one prepared package"
            ));
        }
    }
    if packages.iter().any(|p| p["name"].as_str() == Some(RUNNER)) {
        return Err("prepared lock already contains the reserved plan runner package".into());
    }
    let dependencies = dependencies
        .iter()
        .map(|s| serde_json::to_string(s).unwrap())
        .collect::<Vec<_>>()
        .join(",\n ");
    Ok(format!(
        "{lock}\n[[package]]\nname = \"{RUNNER}\"\nversion = \"0.0.0\"\ndependencies = [\n {dependencies},\n]\n"
    ))
}

pub(crate) fn load(
    root: &Path,
    source: &ProfileSource,
    variant_features: &[String],
    selection: &BuildSelection,
) -> Result<ResolvedPlan, String> {
    let workspace = &source.workspace;
    let runner = workspace.join("plan-runner");
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
    let manifest = format!(
        "[package]\nname = \"{RUNNER}\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[dependencies]\nboard = {{ package = {}, path = {}, default-features = false, features = {} }}\nplatform = {{ package = {}, path = {}, default-features = false, features = [\"host\"] }}\n",
        quoted(&board_name),
        quoted(&board_path),
        serde_json::to_string(variant_features).map_err(|e| e.to_string())?,
        quoted(&platform_name),
        quoted(&platform_path)
    );
    for (path, contents) in [
        (runner.join("Cargo.toml"), manifest.as_str()),
        (
            runner.join("src/main.rs"),
            "fn main() { platform::Plan::<board::Board>::emit(&std::env::args().nth(1).expect(\"missing build selection\")); }\n",
        ),
    ] {
        if fs::read_to_string(&path).ok().as_deref() != Some(contents) {
            fs::write(path, contents).map_err(|e| e.to_string())?;
        }
    }
    let manifest_path = workspace.join("Cargo.toml");
    let workspace_manifest = fs::read_to_string(&manifest_path).map_err(|e| e.to_string())?;
    if !workspace_manifest.contains("members = [") {
        return Err("selected workspace has no member list".into());
    }
    fs::write(
        &manifest_path,
        workspace_manifest.replacen("members = [", "members = [\n  \"plan-runner\",", 1),
    )
    .map_err(|e| e.to_string())?;
    let lock_path = workspace.join("Cargo.lock");
    let before = fs::read_to_string(&lock_path).map_err(|e| e.to_string())?;
    let locked = runner_lock(&before, &[board_name.clone(), platform_name])?;
    fs::write(&lock_path, &locked).map_err(|e| e.to_string())?;
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
    let target = root.join("target/fstart-host").join(&board_name);
    let output = Command::new("cargo")
        .current_dir(root)
        .args(["build", "--locked", "--manifest-path"])
        .arg(&manifest_path)
        .args(["-p", RUNNER, "--target", host, "--target-dir"])
        .arg(target)
        .args(["--message-format=json-render-diagnostics"])
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env("RUSTFLAGS", crate::toolchain::STAGE_ENV_CHECK_CFG)
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
            && let Some(file) = row["executable"].as_str() {
                executables.push(file.to_owned());
            }
    }
    if executables.len() != 1 {
        return Err("Cargo did not report exactly one current host runner executable".into());
    }
    let output = Command::new(&executables[0])
        .arg(serde_json::to_string(selection).map_err(|e| e.to_string())?)
        .current_dir(root)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "platform plan failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|e| format!("platform plan transport: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cargo_accepts_local_runner_entry_on_fresh_and_cached_graphs() {
        let directory =
            std::env::temp_dir().join(format!("fstart-runner-lock-test-{}", std::process::id()));
        if directory.exists() {
            fs::remove_dir_all(&directory).unwrap();
        }
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("Cargo.toml"),
            "[workspace]\nresolver = \"3\"\nmembers = [\"board\", \"platform\"]\n",
        )
        .unwrap();
        for name in ["board", "platform"] {
            let package = directory.join(name);
            fs::create_dir_all(package.join("src")).unwrap();
            fs::write(
                package.join("Cargo.toml"),
                format!(
                    "[package]\nname = \"actual-{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n"
                ),
            )
            .unwrap();
            fs::write(package.join("src/lib.rs"), "pub struct Marker;\n").unwrap();
        }
        let cargo = |args: &[&str]| {
            Command::new("cargo")
                .current_dir(&directory)
                .args(args)
                .env_remove("RUSTFLAGS")
                .env_remove("CARGO_ENCODED_RUSTFLAGS")
                .output()
                .unwrap()
        };
        let prepared = cargo(&["generate-lockfile", "--offline"]);
        assert!(
            prepared.status.success(),
            "{}",
            String::from_utf8_lossy(&prepared.stderr)
        );
        let before = fs::read_to_string(directory.join("Cargo.lock")).unwrap();
        let expected =
            runner_lock(&before, &["actual-board".into(), "actual-platform".into()]).unwrap();
        fs::write(directory.join("Cargo.lock"), &expected).unwrap();
        fs::write(
            directory.join("Cargo.toml"),
            "[workspace]\nresolver = \"3\"\nmembers = [\"board\", \"platform\", \"runner\"]\n",
        )
        .unwrap();
        let runner = directory.join("runner");
        fs::create_dir_all(runner.join("src")).unwrap();
        fs::write(runner.join("Cargo.toml"), "[package]\nname = \"fstart-plan-runner\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[dependencies]\nboard = { package = \"actual-board\", path = \"../board\" }\nplatform = { package = \"actual-platform\", path = \"../platform\" }\n").unwrap();
        fs::write(
            runner.join("src/main.rs"),
            "fn main() { let _ = (board::Marker, platform::Marker); }\n",
        )
        .unwrap();
        for fresh in [false, true] {
            let output = cargo(&[
                "build",
                "--offline",
                "--locked",
                "-p",
                RUNNER,
                "--message-format=json",
            ]);
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let receipts = output
                .stdout
                .split(|b| *b == b'\n')
                .filter(|s| !s.is_empty())
                .map(|s| serde_json::from_slice::<serde_json::Value>(s).unwrap())
                .filter(|r| r["reason"] == "compiler-artifact")
                .collect::<Vec<_>>();
            assert_eq!(receipts.len(), 3);
            assert!(receipts.iter().all(|r| r["fresh"] == fresh));
            assert_eq!(
                fs::read_to_string(directory.join("Cargo.lock")).unwrap(),
                expected
            );
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn runner_entry_uses_package_names_and_preserves_prepared_lock() {
        let original = "version = 4\n[[package]]\nname = \"board-real\"\nversion = \"0.1.0\"\n[[package]]\nname = \"platform-real\"\nversion = \"0.2.0\"\n";
        let augmented =
            runner_lock(original, &["platform-real".into(), "board-real".into()]).unwrap();
        assert!(augmented.starts_with(original));
        let packages = toml::from_str::<toml::Value>(&augmented).unwrap();
        assert_eq!(
            packages["package"][2]["dependencies"][0].as_str(),
            Some("board-real")
        );
        assert_eq!(
            packages["package"][2]["dependencies"][1].as_str(),
            Some("platform-real")
        );
        assert!(runner_lock(&augmented, &["board-real".into()]).is_err());
        assert!(runner_lock(original, &["missing".into()]).is_err());
    }
}
