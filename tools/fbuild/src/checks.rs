//! Architecture policy and extra Cargo checks run by CI (`fbuild checks`).

use std::{path::Path, process::Command};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Build,
    Clippy,
    Test,
}

/// One Cargo invocation per target.
#[derive(Debug, Clone)]
pub struct Check {
    kind: Kind,
    packages: Vec<String>,
    features: Vec<&'static str>,
    no_default_features: bool,
    lib: bool,
    /// Firmware targets, each checked on its own with core and alloc built
    /// from source; the host without any.
    targets: Vec<&'static str>,
    rustflags: Option<&'static str>,
}

impl Check {
    fn new(kind: Kind, packages: &[&str]) -> Self {
        Self {
            kind,
            packages: packages.iter().map(|s| (*s).to_owned()).collect(),
            features: Vec::new(),
            no_default_features: false,
            lib: false,
            targets: Vec::new(),
            rustflags: None,
        }
    }

    fn test(package: &str) -> Self {
        Self::new(Kind::Test, &[package])
    }

    fn clippy(package: &str) -> Self {
        Self::new(Kind::Clippy, &[package])
    }

    fn build(packages: &[&str]) -> Self {
        Self::new(Kind::Build, packages)
    }

    fn features(mut self, features: &[&'static str]) -> Self {
        self.features.extend(features);
        self
    }

    fn no_default_features(mut self) -> Self {
        self.no_default_features = true;
        self
    }

    fn lib(mut self) -> Self {
        self.lib = true;
        self
    }

    fn targets(mut self, targets: &[&'static str]) -> Self {
        self.targets.extend(targets);
        self
    }

    fn rustflags(mut self, flags: &'static str) -> Self {
        self.rustflags = Some(flags);
        self
    }

    pub fn commands(&self) -> Vec<Command> {
        let targets: Vec<Option<&str>> = if self.targets.is_empty() {
            vec![None]
        } else {
            self.targets.iter().copied().map(Some).collect()
        };
        targets
            .into_iter()
            .map(|target| {
                let mut command = Command::new("cargo");
                command.arg(match self.kind {
                    Kind::Build => "build",
                    Kind::Clippy => "clippy",
                    Kind::Test => "test",
                });
                for package in &self.packages {
                    command.args(["-p", package]);
                }
                if self.lib {
                    command.arg("--lib");
                }
                if self.no_default_features {
                    command.arg("--no-default-features");
                }
                if !self.features.is_empty() {
                    command.args(["--features", &self.features.join(",")]);
                }
                if let Some(target) = target {
                    command.args(["--target", target, "-Zbuild-std=core,alloc"]);
                }
                if let Some(flags) = self.rustflags {
                    command.env("RUSTFLAGS", flags);
                }
                command
            })
            .collect()
    }
}

/// Discover workspace members under `crates/platform-*` that expose a `host`
/// feature and a library target, sorted by package name.
fn discover_host_platforms(
    root: &Path,
    metadata: &serde_json::Value,
) -> Result<Vec<String>, String> {
    let members = metadata["workspace_members"]
        .as_array()
        .ok_or("cargo metadata is missing workspace members")?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or("cargo metadata is missing packages")?;
    let crates_root = root
        .join("crates")
        .canonicalize()
        .unwrap_or_else(|_| root.join("crates"));

    let mut platforms = Vec::new();
    for package in packages {
        if !members.contains(&package["id"]) {
            continue;
        }
        let Some(manifest_path) = package["manifest_path"].as_str() else {
            continue;
        };
        let manifest_dir = Path::new(manifest_path)
            .parent()
            .map(|p| p.canonicalize().unwrap_or_else(|_| p.to_path_buf()));
        let is_platform_dir = manifest_dir.as_deref().is_some_and(|dir| {
            dir.strip_prefix(&crates_root).ok().is_some_and(|rel| {
                let mut comps = rel.components();
                match (comps.next(), comps.next()) {
                    (Some(std::path::Component::Normal(name)), None) => {
                        name.to_string_lossy().starts_with("platform-")
                    }
                    _ => false,
                }
            })
        });
        if !is_platform_dir {
            continue;
        }
        let has_host = package["features"]
            .as_object()
            .is_some_and(|features| features.contains_key("host"));
        let has_lib = package["targets"].as_array().is_some_and(|targets| {
            targets.iter().any(|target| {
                target["kind"]
                    .as_array()
                    .is_some_and(|kinds| kinds.iter().any(|kind| kind == "lib"))
            })
        });
        if has_host
            && has_lib
            && let Some(name) = package["name"].as_str()
        {
            platforms.push(name.to_owned());
        }
    }
    platforms.sort();
    Ok(platforms)
}

/// Every check CI runs, with host platform Clippy checks discovered from
/// `metadata`.
pub fn checks_for_workspace(
    root: &Path,
    metadata: &serde_json::Value,
) -> Result<Vec<Check>, String> {
    let mut checks = static_checks();
    // fbuild compiles host plans on demand. Lint all of them here so a
    // broken plan fails the PR rather than the nightly bundle build.
    for platform in discover_host_platforms(root, metadata)? {
        checks.push(
            Check::clippy(&platform)
                .no_default_features()
                .features(&["host"])
                .lib(),
        );
    }
    checks.extend(post_platform_checks());
    Ok(checks)
}

fn static_checks() -> Vec<Check> {
    vec![
        // fstart linked as a library by a foreign firmware (the AST2700
        // BootMCU, riscv32imc) and the ARMv5TE BMC cores: neither has
        // compare-and-swap, and the library build owns no lang items.
        Check::build(&[
            "fstart-core",
            "fstart-log",
            "fstart-timestamp",
            "fstart-crypto",
            "fstart-ffs",
            "fstart-fdt",
            "fstart-store",
            "fstart-load",
            "fstart-stage",
        ])
        .no_default_features()
        .features(&[
            "fstart-stage/ffs",
            "fstart-stage/ed25519",
            "fstart-stage/fdt",
            "fstart-stage/handoff",
            "fstart-stage/alloc",
        ])
        .targets(&["riscv32imc-unknown-none-elf", "armv5te-none-eabi"])
        .rustflags("--cfg portable_atomic_unsafe_assume_single_core"),
        Check::test("fstart-stage").lib().features(&[
            "ffs",
            "ffs-signature",
            "fdt",
            "fit",
            "x86_64",
        ]),
        Check::test("fstart-boot").lib().features(&["crabefi-ui"]),
        Check::test("fstart-driver-intel").lib(),
        Check::test("fstart-platform-intel")
            .lib()
            .features(&["host"]),
        // Without the default consoles, and with the debug ones.
        Check::test("fstart-log").no_default_features(),
        Check::test("fstart-log").features(&["debug-consoles"]),
    ]
}

fn post_platform_checks() -> Vec<Check> {
    let mut checks = Vec::new();
    // Coreboot and bare ELF payloads call the stage's public measured loader,
    // also without a TPM.
    for payload in [&["coreboot"][..], &["coreboot", "tcg"], &["elf"]] {
        checks.push(
            Check::clippy("fstart-payload")
                .lib()
                .features(payload)
                .features(&["fstart-stage/x86_64"]),
        );
    }
    // The AMD drivers and the AMD stage as firmware links them: every IP
    // block and every SoC; a board links what its SoCs reach.
    checks.push(Check::clippy("fstart-driver-amd").targets(&["x86_64-unknown-none"]));
    checks.push(
        Check::clippy("fstart-platform-amd")
            .features(&["bundle"])
            .targets(&["x86_64-unknown-none"]),
    );
    checks
}

const AMD_DRIVER_MANIFEST: &str = "crates/driver-amd/Cargo.toml";

fn workspace_metadata(root: &Path) -> Result<serde_json::Value, String> {
    let output = Command::new("cargo")
        .args([
            "metadata",
            "--no-deps",
            "--format-version=1",
            "--manifest-path",
            "Cargo.toml",
        ])
        .current_dir(root)
        .output()
        .map_err(|e| format!("could not run cargo metadata: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|e| format!("invalid cargo metadata: {e}"))
}

fn check_driver_amd_features_from_metadata(metadata: &serde_json::Value) -> Result<(), String> {
    // `default` is Cargo's selector for a set of features, not a capability.
    const APPROVED: &[&str] = &["acpi", "default", "smbios", "smm", "spi-flash", "testing"];

    // Cargo reports the effective feature names, including implicit features
    // for optional dependencies unless hidden with `dep:`. It need not
    // resolve or build any dependencies to do this.
    let members = metadata["workspace_members"]
        .as_array()
        .ok_or("cargo metadata is missing workspace members")?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or("cargo metadata is missing packages")?;
    let driver = packages
        .iter()
        .find(|p| p["name"] == "fstart-driver-amd" && members.contains(&p["id"]))
        .ok_or_else(|| {
            format!("{AMD_DRIVER_MANIFEST}: fstart-driver-amd is not a workspace member")
        })?;
    let features = driver["features"].as_object().ok_or_else(|| {
        format!("{AMD_DRIVER_MANIFEST}: cargo metadata is missing driver features")
    })?;
    let mut unapproved: Vec<_> = features
        .keys()
        .map(String::as_str)
        .filter(|name| !APPROVED.contains(name))
        .collect();
    if unapproved.is_empty() {
        return Ok(());
    }
    unapproved.sort_unstable();
    Err(format!(
        "{AMD_DRIVER_MANIFEST}: unapproved Cargo features: {}. \
         Keep shared capabilities in driver-amd, select SoCs and IP versions in \
         crates/platform-amd, and put board policy in boards/.",
        unapproved.join(", ")
    ))
}

#[cfg(test)]
fn check_driver_amd_features(root: &Path) -> Result<(), String> {
    check_driver_amd_features_from_metadata(&workspace_metadata(root)?)
}

/// Every Cargo workspace member must inherit `[workspace.lints]` via
/// `[lints] workspace = true`.
fn check_workspace_lints_from_metadata(
    root: &Path,
    metadata: &serde_json::Value,
) -> Result<(), String> {
    let members = metadata["workspace_members"]
        .as_array()
        .ok_or("cargo metadata is missing workspace members")?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or("cargo metadata is missing packages")?;

    let mut checked = 0usize;
    let mut missing = Vec::new();
    for package in packages {
        if !members.contains(&package["id"]) {
            continue;
        }
        let manifest_path = package["manifest_path"]
            .as_str()
            .ok_or("cargo metadata package is missing manifest_path")?;
        checked += 1;
        let cargo_toml = Path::new(manifest_path);
        let text = std::fs::read_to_string(cargo_toml)
            .map_err(|e| format!("failed to read {}: {e}", cargo_toml.display()))?;
        let parsed: toml::Table = toml::from_str(&text)
            .map_err(|e| format!("failed to parse {}: {e}", cargo_toml.display()))?;
        let inherits = parsed
            .get("lints")
            .and_then(|l| l.get("workspace"))
            .and_then(|w| w.as_bool())
            == Some(true);
        if !inherits {
            let rel = cargo_toml
                .strip_prefix(root)
                .unwrap_or(cargo_toml)
                .to_string_lossy()
                .replace('\\', "/");
            missing.push(rel);
        }
    }
    if checked == 0 {
        return Err("cargo metadata reported zero workspace members".to_owned());
    }
    if missing.is_empty() {
        return Ok(());
    }
    missing.sort();
    Err(format!(
        "missing `[lints] workspace = true` in: {}",
        missing.join(", ")
    ))
}

#[cfg(test)]
fn check_workspace_lints(root: &Path) -> Result<(), String> {
    check_workspace_lints_from_metadata(root, &workspace_metadata(root)?)
}

/// `fbuild checks`: run (or list) every check; one failing never stops
/// the others.
pub fn run(list: bool) -> Result<(), String> {
    enum Step {
        DriverAmdFeatures,
        WorkspaceLints,
        Cargo(Command),
    }

    let root = crate::build_board::workspace_root_pub()?;
    let github = std::env::var_os("GITHUB_ACTIONS").is_some();
    let metadata = workspace_metadata(&root);
    if list {
        let metadata = metadata?;
        let cargo_checks = checks_for_workspace(&root, &metadata)?
            .into_iter()
            .flat_map(|check| check.commands())
            .map(Step::Cargo);
        for step in [Step::DriverAmdFeatures, Step::WorkspaceLints]
            .into_iter()
            .chain(cargo_checks)
        {
            match step {
                Step::DriverAmdFeatures => println!("# Check driver-amd capability features"),
                Step::WorkspaceLints => println!("# Check workspace lint inheritance"),
                Step::Cargo(command) => println!("{}", show(&command)),
            }
        }
        return Ok(());
    }
    let mut failed = 0;
    let mut total = 0;
    let cargo_checks = match &metadata {
        Ok(meta) => match checks_for_workspace(&root, meta) {
            Ok(checks) => checks,
            Err(error) => {
                total += 1;
                failed += 1;
                println!("FAIL Discover host platform checks: {error}");
                static_checks()
                    .into_iter()
                    .chain(post_platform_checks())
                    .collect()
            }
        },
        Err(error) => {
            total += 1;
            failed += 1;
            println!("FAIL Discover host platform checks: {error}");
            static_checks()
                .into_iter()
                .chain(post_platform_checks())
                .collect()
        }
    };
    let cargo_checks = cargo_checks
        .into_iter()
        .flat_map(|check| check.commands())
        .map(Step::Cargo);
    let policy_steps = [Step::DriverAmdFeatures, Step::WorkspaceLints];
    for step in policy_steps.into_iter().chain(cargo_checks) {
        total += 1;
        let shown = match &step {
            Step::DriverAmdFeatures => "Check driver-amd capability features".to_owned(),
            Step::WorkspaceLints => "Check workspace lint inheritance".to_owned(),
            Step::Cargo(command) => show(command),
        };
        if github {
            println!("::group::{shown}");
        } else {
            println!("+ {shown}");
        }
        let result = match step {
            Step::DriverAmdFeatures => metadata
                .as_ref()
                .map_err(Clone::clone)
                .and_then(check_driver_amd_features_from_metadata),
            Step::WorkspaceLints => metadata
                .as_ref()
                .map_err(Clone::clone)
                .and_then(|meta| check_workspace_lints_from_metadata(&root, meta)),
            Step::Cargo(mut command) => command
                .current_dir(&root)
                .status()
                .map_err(|e| e.to_string())
                .and_then(|status| {
                    if status.success() {
                        Ok(())
                    } else {
                        Err(status.to_string())
                    }
                }),
        };
        if github {
            println!("::endgroup::");
        }
        if let Err(error) = result {
            println!("FAIL {shown}: {error}");
            if github {
                println!("::error title=fbuild checks::{shown}");
            }
            failed += 1;
        }
    }
    println!("\n{} of {total} checks passed", total - failed);
    match failed {
        0 => Ok(()),
        n => Err(format!("{n} check(s) failed")),
    }
}

fn show(command: &Command) -> String {
    let env = command
        .get_envs()
        .filter_map(|(k, v)| Some(format!("{}='{}' ", k.to_str()?, v?.to_str()?)))
        .collect::<String>();
    let args = command
        .get_args()
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    format!("{env}cargo {args}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(check: &Check) -> Vec<String> {
        check.commands().iter().map(show).collect()
    }

    #[test]
    fn a_check_is_one_cargo_command_per_target() {
        let check = Check::build(&["fstart-core", "fstart-log"])
            .no_default_features()
            .features(&["fstart-log/x"])
            .targets(&["riscv32imc-unknown-none-elf", "armv5te-none-eabi"])
            .rustflags("--cfg single_core");
        assert_eq!(
            shown(&check),
            [
                "RUSTFLAGS='--cfg single_core' cargo build -p fstart-core -p fstart-log \
                 --no-default-features --features fstart-log/x \
                 --target riscv32imc-unknown-none-elf -Zbuild-std=core,alloc",
                "RUSTFLAGS='--cfg single_core' cargo build -p fstart-core -p fstart-log \
                 --no-default-features --features fstart-log/x \
                 --target armv5te-none-eabi -Zbuild-std=core,alloc",
            ]
        );
        assert_eq!(shown(&Check::test("a").lib()), ["cargo test -p a --lib"]);
    }

    fn workspace_root() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    #[test]
    fn workspace_driver_amd_has_only_approved_features() {
        check_driver_amd_features(&workspace_root()).unwrap();
    }

    #[test]
    fn driver_amd_guard_checks_explicit_and_implicit_cargo_features() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/lib.rs"), "").unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            r#"
[package]
name = "fstart-driver-amd"
version = "0.1.0"

[features]
default = ["acpi"]
acpi = ["dep:hidden", "forwarded/extra", "weak?/extra"]
zen6 = []

[dependencies]
hidden = { package = "private-thing", version = "1", optional = true }
forwarded = { package = "some-lib", version = "1", optional = true }
weak = { package = "another-lib", version = "1", optional = true }
required = "1"

[target.'cfg(target_os = "none")'.build-dependencies]
target-alias = { package = "build-tool", version = "1", optional = true }

[dev-dependencies]
testing-tool = "1"
"#,
        )
        .unwrap();

        let error = check_driver_amd_features(tmp.path()).unwrap_err();
        assert!(
            error.starts_with(
                "crates/driver-amd/Cargo.toml: unapproved Cargo features: forwarded, target-alias, weak, zen6."
            ),
            "{error}"
        );
        assert!(error.contains("crates/platform-amd") && error.contains("boards/"));
    }

    #[test]
    fn workspace_members_inherit_workspace_lints() {
        check_workspace_lints(&workspace_root()).unwrap();
    }

    #[test]
    fn workspace_lints_guard_uses_cargo_membership_and_rejects_missing_inheritance() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            r#"
[workspace]
resolver = "3"
members = ["crates/ok", "extra/missing", "boards/vendor/local-override"]
exclude = ["crates/excluded"]

[workspace.lints.rust]
unused_must_use = "deny"
"#,
        )
        .unwrap();
        for (dir, manifest) in [
            (
                "crates/ok",
                "[package]\nname = \"ok\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[lints]\nworkspace = true\n",
            ),
            (
                "extra/missing",
                "[package]\nname = \"missing\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
            ),
            (
                "boards/vendor/local-override",
                "[package]\nname = \"local-override\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[lints.rust]\nunused_must_use = \"warn\"\n",
            ),
            (
                "crates/excluded",
                "[package]\nname = \"excluded\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
            ),
        ] {
            let src = tmp.path().join(dir).join("src");
            std::fs::create_dir_all(&src).unwrap();
            std::fs::write(src.join("lib.rs"), "").unwrap();
            std::fs::write(tmp.path().join(dir).join("Cargo.toml"), manifest).unwrap();
        }
        let err = check_workspace_lints(tmp.path()).unwrap_err();
        assert!(err.contains("extra/missing/Cargo.toml"), "{err}");
        assert!(
            err.contains("boards/vendor/local-override/Cargo.toml"),
            "{err}"
        );
        assert!(!err.contains("crates/ok/Cargo.toml"), "{err}");
        assert!(!err.contains("crates/excluded/Cargo.toml"), "{err}");
    }

    #[test]
    fn every_check_names_a_workspace_package() {
        let root = workspace_root();
        let metadata = workspace_metadata(&root).unwrap();
        let members: Vec<_> = metadata["packages"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|package| package["name"].as_str())
            .collect();
        let checks = checks_for_workspace(&root, &metadata).unwrap();
        for check in &checks {
            for package in &check.packages {
                assert!(
                    members.contains(&package.as_str()),
                    "{package} is no workspace member"
                );
            }
        }
    }

    #[test]
    fn discovers_sorted_host_platform_members_only() {
        let root = Path::new("/ws");
        let metadata = serde_json::json!({
            "workspace_members": ["p-z", "p-a", "p-nohost", "core"],
            "packages": [
                {
                    "id": "p-z",
                    "name": "fstart-platform-z",
                    "manifest_path": "/ws/crates/platform-z/Cargo.toml",
                    "features": {"host": []},
                    "targets": [{"kind": ["lib"]}]
                },
                {
                    "id": "p-a",
                    "name": "fstart-platform-a",
                    "manifest_path": "/ws/crates/platform-a/Cargo.toml",
                    "features": {"host": []},
                    "targets": [{"kind": ["lib"]}]
                },
                {
                    "id": "p-nohost",
                    "name": "fstart-platform-nohost",
                    "manifest_path": "/ws/crates/platform-nohost/Cargo.toml",
                    "features": {},
                    "targets": [{"kind": ["lib"]}]
                },
                {
                    "id": "p-excluded",
                    "name": "fstart-platform-excluded",
                    "manifest_path": "/ws/crates/platform-excluded/Cargo.toml",
                    "features": {"host": []},
                    "targets": [{"kind": ["lib"]}]
                },
                {
                    "id": "core",
                    "name": "fstart-core",
                    "manifest_path": "/ws/crates/core/Cargo.toml",
                    "features": {"host": []},
                    "targets": [{"kind": ["lib"]}]
                }
            ]
        });
        assert_eq!(
            discover_host_platforms(root, &metadata).unwrap(),
            vec!["fstart-platform-a", "fstart-platform-z"]
        );
    }
}
