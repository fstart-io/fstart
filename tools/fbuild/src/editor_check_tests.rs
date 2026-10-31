//! Exercise the recorded editor helper itself, not just its command spelling.
use crate::{board_manifest, build_board, selection::Selection};
use std::{collections::BTreeMap, fs, path::Path, process::Command};

#[test]
fn diagnostics_reach_stdout() {
    if let Some(path) = std::env::var_os("EDITOR_SELECTION_PROBE") {
        assert!(Selection::editor_check(Path::new(&path)).is_err());
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let actual = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    fs::copy(
        actual.join("rust-toolchain.toml"),
        root.join("rust-toolchain.toml"),
    )
    .unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nresolver='3'\nmembers=['boards/test/broken']\n",
    )
    .unwrap();
    let board = root.join("boards/test/broken");
    fs::create_dir_all(board.join("src")).unwrap();
    fs::write(board.join("Cargo.toml"), "[package]\nname='broken'\nversion='0.1.0'\nedition='2024'\n[package.metadata.fstart]\nschema=1\nboard='broken'\nstage-bin='fstart-stage'\n[[bin]]\nname='fstart-stage'\npath='src/main.rs'\n").unwrap();
    fs::write(
        board.join("src/main.rs"),
        "fn main() { let _: u8 = \"not a number\"; }",
    )
    .unwrap();
    let lock = Command::new("cargo")
        .current_dir(root)
        .args(["generate-lockfile", "--offline"])
        .output()
        .unwrap();
    assert!(
        lock.status.success(),
        "{}",
        String::from_utf8_lossy(&lock.stderr)
    );
    let board = board_manifest::find(root, "broken").unwrap();
    build_board::prepare_selected_board_workspace(root, &board).unwrap();
    let rustc = Command::new("rustc")
        .current_dir(root)
        .arg("-vV")
        .output()
        .unwrap();
    let rustc = String::from_utf8(rustc.stdout).unwrap();
    let host = rustc
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .unwrap();
    let mut unit = fstart_platform_qemu::host::resolve(
        fstart_platform_qemu::facts::VirtMachine::Riscv64,
        fstart_image_build::plan::BuildSelection {
            payload: Some("halt".into()),
            ..Default::default()
        },
    )
    .unwrap()
    .units
    .remove(0);
    unit.target = host.into();
    unit.features.clear();
    unit.rustflags.clear();
    unit.build_std = None;
    unit.linker_script = None;
    let selection = Selection::prepare_unit(
        root,
        &board,
        &unit,
        &Default::default(),
        root.join("selection"),
        "{}",
        BTreeMap::new(),
        &[],
        false,
    )
    .unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "editor_check_tests::diagnostics_reach_stdout",
            "--nocapture",
        ])
        .env(
            "EDITOR_SELECTION_PROBE",
            selection.directory.join("editor-selection.json"),
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output
            .stdout
            .split(|&byte| byte == b'\n')
            .filter_map(|line| serde_json::from_slice::<serde_json::Value>(line).ok())
            .any(|row| row["reason"] == "compiler-message"
                && row["message"]["code"]["code"] == "E0308"),
        "editor received no JSON type error on stdout:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
