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
    packages: Vec<&'static str>,
    features: Vec<&'static str>,
    no_default_features: bool,
    lib: bool,
    /// Firmware targets, each checked on its own with core and alloc built
    /// from source; the host without any.
    targets: Vec<&'static str>,
    rustflags: Option<&'static str>,
}

impl Check {
    fn new(kind: Kind, packages: &[&'static str]) -> Self {
        Self {
            kind,
            packages: packages.to_vec(),
            features: Vec::new(),
            no_default_features: false,
            lib: false,
            targets: Vec::new(),
            rustflags: None,
        }
    }

    fn test(package: &'static str) -> Self {
        Self::new(Kind::Test, &[package])
    }

    fn clippy(package: &'static str) -> Self {
        Self::new(Kind::Clippy, &[package])
    }

    fn build(packages: &[&'static str]) -> Self {
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

/// Every check CI runs.
pub fn checks() -> Vec<Check> {
    let mut checks = vec![
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
    ];
    // fbuild compiles host plans on demand. Lint all of them here so a
    // broken plan fails the PR rather than the nightly bundle build.
    for platform in [
        "fstart-platform-amd",
        "fstart-platform-aspeed",
        "fstart-platform-intel",
        "fstart-platform-qemu",
        "fstart-platform-sifive",
        "fstart-platform-sunxi",
    ] {
        checks.push(Check::clippy(platform).features(&["host"]).lib());
    }
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

fn check_driver_amd_features(root: &Path) -> Result<(), String> {
    // `default` is Cargo's selector for a set of features, not a capability.
    const APPROVED: &[&str] = &["acpi", "default", "smbios", "smm", "spi-flash", "testing"];

    // Cargo reports the effective feature names, including implicit features
    // for optional dependencies unless hidden with `dep:`. It need not
    // resolve or build any dependencies to do this.
    let metadata = workspace_metadata(root)?;
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

/// `fbuild checks`: run (or list) every check; one failing never stops
/// the others.
pub fn run(list: bool) -> Result<(), String> {
    enum Step {
        DriverAmdFeatures,
        Cargo(Command),
    }

    let root = crate::build_board::workspace_root_pub()?;
    let github = std::env::var_os("GITHUB_ACTIONS").is_some();
    let cargo_checks = checks()
        .into_iter()
        .flat_map(|check| check.commands())
        .map(Step::Cargo);
    let mut failed = 0;
    let mut total = 0;
    for step in std::iter::once(Step::DriverAmdFeatures).chain(cargo_checks) {
        total += 1;
        let shown = match &step {
            Step::DriverAmdFeatures => "Check driver-amd capability features".to_owned(),
            Step::Cargo(command) => show(command),
        };
        if list {
            match step {
                Step::DriverAmdFeatures => println!("# {shown}"),
                Step::Cargo(_) => println!("{shown}"),
            }
            continue;
        }
        if github {
            println!("::group::{shown}");
        } else {
            println!("+ {shown}");
        }
        let result = match step {
            Step::DriverAmdFeatures => check_driver_amd_features(&root),
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
    if list {
        return Ok(());
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
    fn every_check_names_a_workspace_package() {
        let metadata = workspace_metadata(&workspace_root()).unwrap();
        let members: Vec<_> = metadata["packages"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|package| package["name"].as_str())
            .collect();
        for check in checks() {
            for package in &check.packages {
                assert!(
                    members.contains(package),
                    "{package} is no workspace member"
                );
            }
        }
    }
}
