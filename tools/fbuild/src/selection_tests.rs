use super::Selection;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn ambient_artifact_environment_isolation() {
    if std::env::var_os("SELECTION_ENV_PROBE").is_none() {
        // Subprocess isolation avoids unsafe process-global environment mutation
        // while the test runner is executing other tests in parallel.
        for unknown in [false, true] {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child
                .args([
                    "--exact",
                    "selection::tests::ambient_artifact_environment_isolation",
                    "--nocapture",
                ])
                .env("SELECTION_ENV_PROBE", "1")
                .env("FSTART_FIXTURE_IMAGE", "unrecorded-by-consumer")
                .env("AUX_INPUT", "another-leak");
            if unknown {
                child.env("FSTART_UNKNOWN_PRODUCER", "leak");
            }
            assert!(child.status().unwrap().success());
        }
        return;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let board = crate::board_manifest::find(root, "qemu-aarch64").unwrap();
    let scratch =
        Scratch(std::env::temp_dir().join(format!("fstart env isolation {}", std::process::id())));
    let unit = fstart_platform_qemu::host::resolve(
        fstart_platform_qemu::facts::VirtMachine::Aarch64,
        fstart_image_build::plan::BuildSelection {
            payload: Some("halt".into()),
            ..Default::default()
        },
    )
    .unwrap()
    .units
    .remove(0);
    let keys = vec!["FSTART_FIXTURE_IMAGE".into(), "AUX_INPUT".into()];
    let prepare = |env| {
        Selection::prepare_unit(
            root,
            &board,
            &unit,
            scratch.0.clone(),
            "{}",
            env,
            &keys,
            true,
        )
    };
    if std::env::var_os("FSTART_UNKNOWN_PRODUCER").is_some() {
        assert!(
            prepare(BTreeMap::new())
                .err()
                .unwrap()
                .contains("unrecorded ambient")
        );
        return;
    }
    let unbound = prepare(BTreeMap::new()).unwrap();
    assert_eq!(unbound.manifest, root.join("Cargo.toml"));
    assert!(
        unbound
            .args
            .windows(2)
            .any(|args| { args[0] == "--package" && args[1] == board.package })
    );
    let mut command = std::process::Command::new("env");
    unbound.apply_environment(&mut command);
    let output = String::from_utf8(command.output().unwrap().stdout).unwrap();
    assert!(!output.contains("FSTART_FIXTURE_IMAGE=") && !output.contains("AUX_INPUT="));
    assert_eq!(
        &unbound.check_command()[..5],
        ["env", "-u", "FSTART_FIXTURE_IMAGE", "-u", "AUX_INPUT"]
    );
    let bound = prepare(BTreeMap::from([(
        "FSTART_FIXTURE_IMAGE".into(),
        "recorded".into(),
    )]))
    .unwrap();
    let mut command = std::process::Command::new("env");
    bound.apply_environment(&mut command);
    let output = String::from_utf8(command.output().unwrap().stdout).unwrap();
    assert!(output.contains("FSTART_FIXTURE_IMAGE=recorded\n") && !output.contains("AUX_INPUT="));
}

/// Profile rustflags the unit applies to the board package being linked.
pub(crate) fn board_flags(selection: &Selection) -> Vec<String> {
    let recorded: serde_json::Value =
        serde_json::from_slice(&fs::read(selection.directory.join("rustflags.json")).unwrap())
            .unwrap();
    serde_json::from_value(recorded["board_rustflags"].clone()).unwrap()
}

#[test]
fn linker_script_rustflags_are_scoped_to_board_package() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let board = crate::board_manifest::find(root, "qemu-aarch64").unwrap();
    let unit = fstart_platform_qemu::host::resolve(
        fstart_platform_qemu::facts::VirtMachine::Aarch64,
        fstart_image_build::plan::BuildSelection {
            payload: Some("halt".into()),
            ..Default::default()
        },
    )
    .unwrap()
    .units
    .remove(0);
    let scratch =
        Scratch(std::env::temp_dir().join(format!("fstart profile scope {}", std::process::id())));
    let selection = Selection::prepare_unit(
        root,
        &board,
        &unit,
        scratch.0.clone(),
        "{}",
        BTreeMap::new(),
        &[],
        true,
    )
    .unwrap();
    let profile_toml = fs::read_to_string(selection.directory.join("profile.toml")).unwrap();
    let parsed: toml::Value = toml::from_str(&profile_toml).unwrap();
    let release = &parsed["profile"]["release"];
    assert!(
        release.get("rustflags").is_none(),
        "workspace-wide profile rustflags would bust Cargo and sccache for every driver/platform crate: {profile_toml}"
    );
    let pkg_flags = release["package"][board.package.as_str()]["rustflags"]
        .as_array()
        .expect("board package should carry the linker script rustflags");
    assert_eq!(pkg_flags.len(), 1);
    assert!(pkg_flags[0].as_str().unwrap().starts_with("-Clink-arg=-T"));

    // Distinct global rustflags (e.g. pic vs static relocation models) must
    // use distinct Cargo target directories so stages do not clobber each
    // other's core/driver artifacts in place, while identical flags share one.
    let same = Selection::prepare_unit(
        root,
        &board,
        &unit,
        scratch.0.join("same"),
        "{}",
        BTreeMap::new(),
        &[],
        true,
    )
    .unwrap();
    assert_eq!(selection.target_dir(root), same.target_dir(root));

    let mut other_unit = unit;
    other_unit.rustflags.push("-Crelocation-model=pic".into());
    let other = Selection::prepare_unit(
        root,
        &board,
        &other_unit,
        scratch.0.join("other"),
        "{}",
        BTreeMap::new(),
        &[],
        true,
    )
    .unwrap();
    assert_ne!(selection.target_dir(root), other.target_dir(root));
}

/// The path rustc records for `path` under `flags`: the last matching
/// `--remap-path-prefix` wins, matching whole components.
fn remapped(flags: &[String], path: &Path) -> PathBuf {
    flags
        .iter()
        .rev()
        .filter_map(|flag| flag.strip_prefix("--remap-path-prefix="))
        .filter_map(|mapping| mapping.rsplit_once('='))
        .find_map(|(from, to)| {
            path.strip_prefix(from)
                .ok()
                .map(|rest| Path::new(to).join(rest))
        })
        .unwrap_or_else(|| path.to_path_buf())
}

#[test]
fn remap_path_prefixes_cover_workspace_cargo_and_rustup() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let board = crate::board_manifest::find(root, "qemu-aarch64").unwrap();
    let unit = fstart_platform_qemu::host::resolve(
        fstart_platform_qemu::facts::VirtMachine::Aarch64,
        fstart_image_build::plan::BuildSelection {
            payload: Some("halt".into()),
            ..Default::default()
        },
    )
    .unwrap()
    .units
    .remove(0);
    let sysroot = super::rustc_sysroot().expect("rustc --print sysroot");

    if std::env::var_os("SELECTION_REMAP_PROBE").is_none() {
        // Nest the prefixes as rustup and a CI cache do: the sysroot inside
        // RUSTUP_HOME, CARGO_HOME inside the workspace.
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "selection::tests::remap_path_prefixes_cover_workspace_cargo_and_rustup",
                "--nocapture",
            ])
            .env("SELECTION_REMAP_PROBE", "1")
            .env("CARGO_HOME", root.join("target/remap-cargo-home"))
            .env("RUSTUP_HOME", sysroot.parent().unwrap().parent().unwrap());
        assert!(child.status().unwrap().success());
        return;
    }
    let scratch =
        Scratch(std::env::temp_dir().join(format!("fstart remap probe {}", std::process::id())));
    let selection = Selection::prepare_unit(
        root,
        &board,
        &unit,
        scratch.0.clone(),
        "{}",
        BTreeMap::new(),
        &[],
        true,
    )
    .unwrap();
    // Check/IDE environment must NOT rewrite diagnostic paths.
    let check_flags: Vec<_> = selection.environment()["CARGO_ENCODED_RUSTFLAGS"]
        .split('\u{1f}')
        .map(str::to_string)
        .collect();
    assert!(
        !check_flags
            .iter()
            .any(|f| f.starts_with("--remap-path-prefix="))
    );
    let build_flags: Vec<_> = selection.build_environment()["CARGO_ENCODED_RUSTFLAGS"]
        .split('\u{1f}')
        .map(str::to_string)
        .collect();
    // The most specific prefix wins for every nested location.
    let rustup_home = sysroot.parent().unwrap().parent().unwrap();
    for (path, expected) in [
        (
            root.join("crates/core/src/lib.rs"),
            "/fstart/crates/core/src/lib.rs",
        ),
        (
            root.join("target/remap-cargo-home/registry/src/x/lib.rs"),
            "/cargo/registry/src/x/lib.rs",
        ),
        (
            sysroot.join("lib/rustlib/src/rust/library/core/src/lib.rs"),
            "/rustc/library/core/src/lib.rs",
        ),
        (
            sysroot.join("lib/rustlib/x"),
            "/rustc-sysroot/lib/rustlib/x",
        ),
        (rustup_home.join("settings.toml"), "/rustup/settings.toml"),
    ] {
        assert_eq!(
            remapped(&build_flags, &path),
            Path::new(expected),
            "{path:?}"
        );
    }
    let recorded: serde_json::Value =
        serde_json::from_slice(&fs::read(selection.directory.join("rustflags.json")).unwrap())
            .unwrap();
    let recorded: Vec<String> =
        serde_json::from_value(recorded["build_remap_rustflags"].clone()).unwrap();
    assert!(recorded.iter().all(|flag| build_flags.contains(flag)));
    assert!(!recorded.is_empty());
    assert!(
        selection
            .args
            .windows(2)
            .any(|args| args == ["-Z", "build-std-features=optimize_for_size"])
    );
}
