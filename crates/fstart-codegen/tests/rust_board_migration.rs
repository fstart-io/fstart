//! Migration safety checks for Rust board crate metadata.
//!
//! These tests keep the Rust board crate path honest. Boards own their metadata
//! and expose it through the standard host metadata binary, while host tooling
//! must not link every concrete board crate directly.

use std::path::{Path, PathBuf};

use fstart_codegen::board_loader::load_parsed_board_metadata_only;
use fstart_types::{BoardConfig, BuildInfo, DeviceRole, Platform};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crate is under crates/fstart-codegen")
        .to_path_buf()
}

#[derive(Clone, Copy)]
struct RustBoardCase {
    board: &'static str,
    package: &'static str,
    platform: Platform,
    feature: &'static str,
    driver_feature: &'static str,
    root_device: &'static str,
    device_count: usize,
    board_config: fn() -> BoardConfig,
    build_info: fn() -> BuildInfo,
}

const RUST_BOARD_CASES: &[RustBoardCase] = &[RustBoardCase {
    board: "lenovo-x61",
    package: "fstart-board-lenovo-x61",
    platform: Platform::X86_64,
    feature: "x86_64",
    driver_feature: "intel-gm965",
    root_device: "northbridge",
    device_count: 14,
    board_config: fstart_board_lenovo_x61::board_config,
    build_info: fstart_board_lenovo_x61::build_info,
}];

#[test]
fn rust_boards_parse_from_direct_rust_metadata() {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            for case in RUST_BOARD_CASES {
                let parsed = load_parsed_board_metadata_only((case.board_config)(), Vec::new())
                    .unwrap_or_else(|e| {
                        panic!(
                            "Rust-authored {} metadata parses through codegen: {e}",
                            case.board
                        )
                    });

                assert_eq!(parsed.config.name.as_str(), case.board);
                assert_eq!(parsed.config.platform, case.platform);
                assert_eq!(parsed.config.devices.len(), case.device_count);
                assert_eq!(parsed.config.devices[0].name.as_str(), case.root_device);
                if case.board == "lenovo-x61" {
                    assert_device_role(&parsed.config, "pcie1", DeviceRole::PciBridge, true);
                    assert_device_role(&parsed.config, "pcie2", DeviceRole::PciBridge, true);
                    assert_device_role(&parsed.config, "pcie3", DeviceRole::PciBridge, false);
                    assert_device_role(&parsed.config, "pcie4", DeviceRole::PciBridge, false);
                    assert_device_role(&parsed.config, "pcie5", DeviceRole::PciBridge, false);
                    assert_device_role(&parsed.config, "pcie6", DeviceRole::PciBridge, false);
                    assert_device_role(&parsed.config, "dock_superio", DeviceRole::Runtime, false);
                    assert_device_role(&parsed.config, "dlpc_superio", DeviceRole::Runtime, true);
                    assert_device_role(&parsed.config, "ck505", DeviceRole::Runtime, false);
                }
            }
        })
        .expect("spawn board metadata test")
        .join()
        .expect("board metadata test panicked");
}

fn assert_device_role(config: &BoardConfig, name: &str, role: DeviceRole, enabled: bool) {
    let device = config
        .devices
        .iter()
        .find(|device| device.name.as_str() == name)
        .unwrap_or_else(|| panic!("{} should declare device {name}", config.name));
    assert_eq!(device.role, role, "device {name} role");
    assert_eq!(device.enabled, enabled, "device {name} enabled policy");
}

#[test]
fn rust_boards_emit_build_info_from_direct_rust_metadata() {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            for case in RUST_BOARD_CASES {
                let build_info = (case.build_info)();

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
        })
        .expect("spawn board build-info test")
        .join()
        .expect("board build-info test panicked");
}

#[test]
fn xtask_does_not_link_concrete_board_crates() {
    let manifest =
        std::fs::read_to_string(repo_root().join("xtask/Cargo.toml")).expect("read xtask manifest");

    for case in RUST_BOARD_CASES {
        assert!(
            !manifest.contains(case.package),
            "xtask must discover {} through its board metadata binary, not link {} directly",
            case.board,
            case.package
        );
    }
}
