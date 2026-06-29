//! Migration safety checks for Rust board crate metadata.
//!
//! These tests keep the opt-in Rust board crate path honest without requiring
//! every legacy `board.ron` board to be ported at once. Rust boards must expose
//! normal Rust functions. They must not require helper binaries or serialized
//! board metadata transport.

use std::path::{Path, PathBuf};

use fstart_codegen::ron_loader::load_parsed_board_from_rust;
use fstart_device_registry::DriverBinding;
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
    binding_count: usize,
    parsed_driver_count: usize,
    board_config: fn() -> BoardConfig,
    driver_bindings: fn() -> Vec<DriverBinding>,
    build_info: fn() -> BuildInfo,
}

const RUST_BOARD_CASES: &[RustBoardCase] = &[
    RustBoardCase {
        board: "qemu-riscv64",
        package: "fstart-board-qemu-riscv64",
        platform: Platform::Riscv64,
        feature: "riscv64",
        driver_feature: "ns16550",
        root_device: "uart0",
        device_count: 1,
        binding_count: 1,
        parsed_driver_count: 1,
        board_config: fstart_board_qemu_riscv64::board_config,
        driver_bindings: fstart_board_qemu_riscv64::driver_bindings,
        build_info: fstart_board_qemu_riscv64::build_info,
    },
    RustBoardCase {
        board: "qemu-aarch64",
        package: "fstart-board-qemu-aarch64",
        platform: Platform::Aarch64,
        feature: "aarch64",
        driver_feature: "pl011",
        root_device: "uart0",
        device_count: 1,
        binding_count: 1,
        parsed_driver_count: 1,
        board_config: fstart_board_qemu_aarch64::board_config,
        driver_bindings: fstart_board_qemu_aarch64::driver_bindings,
        build_info: fstart_board_qemu_aarch64::build_info,
    },
    RustBoardCase {
        board: "foxconn-d41s",
        package: "fstart-board-foxconn-d41s",
        platform: Platform::X86_64,
        feature: "x86_64",
        driver_feature: "intel-pineview",
        root_device: "northbridge",
        device_count: 10,
        binding_count: 4,
        parsed_driver_count: 10,
        board_config: fstart_board_foxconn_d41s::board_config,
        driver_bindings: fstart_board_foxconn_d41s::driver_bindings,
        build_info: fstart_board_foxconn_d41s::build_info,
    },
    RustBoardCase {
        board: "lenovo-x61",
        package: "fstart-board-lenovo-x61",
        platform: Platform::X86_64,
        feature: "x86_64",
        driver_feature: "intel-gm965",
        root_device: "northbridge",
        device_count: 15,
        binding_count: 7,
        parsed_driver_count: 15,
        board_config: fstart_board_lenovo_x61::board_config,
        driver_bindings: fstart_board_lenovo_x61::driver_bindings,
        build_info: fstart_board_lenovo_x61::build_info,
    },
    RustBoardCase {
        board: "foxconn-d41s-uefi",
        package: "fstart-board-foxconn-d41s-uefi",
        platform: Platform::X86_64,
        feature: "x86_64",
        driver_feature: "intel-pineview",
        root_device: "northbridge",
        device_count: 10,
        binding_count: 4,
        parsed_driver_count: 10,
        board_config: fstart_board_foxconn_d41s_uefi::board_config,
        driver_bindings: fstart_board_foxconn_d41s_uefi::driver_bindings,
        build_info: fstart_board_foxconn_d41s_uefi::build_info,
    },
];

#[test]
fn rust_boards_parse_from_direct_rust_metadata() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            for case in RUST_BOARD_CASES {
                let driver_bindings = (case.driver_bindings)();
                assert_eq!(driver_bindings.len(), case.binding_count);

                let parsed = load_parsed_board_from_rust((case.board_config)(), driver_bindings)
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
                assert_eq!(parsed.driver_instances.len(), case.parsed_driver_count);
                if case.board == "lenovo-x61" {
                    assert_device_role(&parsed.config, "pcie1", DeviceRole::PciBridge, true);
                    assert_device_role(&parsed.config, "pcie2", DeviceRole::PciBridge, true);
                    assert_device_role(&parsed.config, "pcie3", DeviceRole::PciBridge, false);
                    assert_device_role(&parsed.config, "pcie4", DeviceRole::PciBridge, false);
                    assert_device_role(&parsed.config, "pcie5", DeviceRole::PciBridge, false);
                    assert_device_role(&parsed.config, "pcie6", DeviceRole::PciBridge, false);
                    assert_device_role(&parsed.config, "dock_superio", DeviceRole::Runtime, false);
                    assert_device_role(&parsed.config, "ck505", DeviceRole::Runtime, false);
                }
                if case.board.starts_with("foxconn-d41s") {
                    assert_device_role(&parsed.config, "pcie0", DeviceRole::PciBridge, true);
                    assert_device_role(&parsed.config, "pcie1", DeviceRole::PciBridge, true);
                    assert_device_role(&parsed.config, "pcie2", DeviceRole::PciBridge, false);
                    assert_device_role(&parsed.config, "pcie3", DeviceRole::PciBridge, false);
                    assert_device_role(&parsed.config, "lpc", DeviceRole::LpcBus, true);
                    assert_device_role(&parsed.config, "smbus", DeviceRole::SmBus, true);
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
}

#[test]
fn rust_board_crates_do_not_define_metadata_helper_binaries() {
    let root = repo_root();
    for case in RUST_BOARD_CASES {
        let helper = root
            .join("boards")
            .join(case.board)
            .join("src")
            .join("main.rs");
        assert!(
            !helper.exists(),
            "{} must expose Rust metadata directly, not a serialized helper binary at {}",
            case.board,
            helper.display()
        );
    }
}
