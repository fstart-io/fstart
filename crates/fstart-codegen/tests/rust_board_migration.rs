//! Migration safety checks for Rust board crate metadata.
//!
//! These tests keep the opt-in Rust board crate path honest without requiring
//! every legacy `board.ron` board to be ported at once.

use std::path::{Path, PathBuf};
use std::process::Command;

use fstart_codegen::ron_loader::load_parsed_board_from_str;
use fstart_types::{BuildInfo, Platform};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crate is under crates/fstart-codegen")
        .to_path_buf()
}

fn helper(package: &str, command: &str) -> String {
    let output = Command::new("cargo")
        .arg("run")
        .arg("--quiet")
        .arg("--package")
        .arg(package)
        .arg("--")
        .arg(command)
        .current_dir(repo_root())
        .output()
        .unwrap_or_else(|e| panic!("run {package} metadata helper: {e}"));

    assert!(
        output.status.success(),
        "{package} helper failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("helper output is UTF-8")
}

#[derive(Clone, Copy)]
struct RustBoardCase {
    board: &'static str,
    package: &'static str,
    platform: Platform,
    feature: &'static str,
    driver_feature: &'static str,
}

const RUST_BOARD_CASES: &[RustBoardCase] = &[
    RustBoardCase {
        board: "qemu-riscv64",
        package: "fstart-board-qemu-riscv64",
        platform: Platform::Riscv64,
        feature: "riscv64",
        driver_feature: "ns16550",
    },
    RustBoardCase {
        board: "qemu-aarch64",
        package: "fstart-board-qemu-aarch64",
        platform: Platform::Aarch64,
        feature: "aarch64",
        driver_feature: "pl011",
    },
];

#[test]
fn rust_boards_emit_parseable_codegen_metadata() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            for case in RUST_BOARD_CASES {
                let board_config = helper(case.package, "board-config");
                assert!(
                    !board_config.contains("BOARD_CONFIG_RON"),
                    "{} helper output should be serialized metadata, not an embedded source constant",
                    case.board
                );
                let parsed = load_parsed_board_from_str(
                    &board_config,
                    &format!("{} board-config", case.board),
                )
                .unwrap_or_else(|e| {
                    panic!(
                        "Rust-authored {} metadata parses through codegen: {e}",
                        case.board
                    )
                });

                assert_eq!(parsed.config.name.as_str(), case.board);
                assert_eq!(parsed.config.platform, case.platform);
                assert_eq!(parsed.config.devices.len(), 1);
                assert_eq!(parsed.config.devices[0].name.as_str(), "uart0");
                assert_eq!(parsed.driver_instances.len(), 1);
            }
        })
        .expect("spawn board metadata test")
        .join()
        .expect("board metadata test panicked");
}

#[test]
fn rust_boards_emit_build_info() {
    for case in RUST_BOARD_CASES {
        let build_info: BuildInfo = ron::Options::default()
            .from_str(&helper(case.package, "build-info"))
            .expect("build-info should be RON-serialized BuildInfo");

        assert_eq!(build_info.name.as_str(), case.board);
        assert_eq!(build_info.board_package.as_str(), case.package);
        assert_eq!(build_info.target.as_str(), case.platform.target_triple());
        assert!(build_info
            .features
            .iter()
            .any(|feature| feature == case.feature));
        assert!(build_info
            .features
            .iter()
            .any(|feature| feature == case.driver_feature));
    }
}
