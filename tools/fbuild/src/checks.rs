//! Feature selections and targets CI builds, lints or tests beyond the
//! workspace's default `cargo test` and `cargo clippy` (`fbuild checks`).

use std::process::Command;

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

/// `fbuild checks`: run (or `list`) every check; one failing never stops
/// the others.
pub fn run(list: bool) -> Result<(), String> {
    let root = crate::build_board::workspace_root_pub()?;
    let github = std::env::var_os("GITHUB_ACTIONS").is_some();
    let mut failed = Vec::new();
    let mut total = 0;
    for check in checks() {
        for mut command in check.commands() {
            total += 1;
            let shown = show(&command);
            if list {
                println!("{shown}");
                continue;
            }
            if github {
                println!("::group::{shown}");
            } else {
                println!("+ {shown}");
            }
            let status = command.current_dir(&root).status();
            if github {
                println!("::endgroup::");
            }
            if !status.as_ref().is_ok_and(|status| status.success()) {
                println!("FAIL {shown}");
                if github {
                    println!("::error title=fbuild checks::{shown}");
                }
                failed.push(shown);
            }
        }
    }
    if list {
        return Ok(());
    }
    println!("\n{} of {total} checks passed", total - failed.len());
    match failed.len() {
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

    #[test]
    fn every_check_names_a_workspace_package() {
        let output = Command::new("cargo")
            .args(["metadata", "--no-deps", "--format-version=1"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .unwrap();
        let metadata: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
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
