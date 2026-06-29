//! Direct access to migrated Rust board crates.
//!
//! This is the build-time bridge while boards are still being migrated one by one.
//! It deliberately calls board crate Rust functions directly: no helper binary, no
//! RON/JSON/postcard transport.

use fstart_codegen::ron_loader::{load_parsed_board_from_rust, ParsedBoard};
use fstart_device_registry::DriverBinding;
use fstart_types::{BoardConfig, BuildInfo};

pub fn parsed_board(board: &str) -> Option<Result<ParsedBoard, String>> {
    let (config, driver_bindings) = board_config_and_driver_bindings(board)?;
    Some(load_parsed_board_from_rust(config, driver_bindings))
}

pub fn build_info(board: &str) -> Option<BuildInfo> {
    Some(match board {
        "qemu-riscv64" => fstart_board_qemu_riscv64::build_info(),
        "qemu-aarch64" => fstart_board_qemu_aarch64::build_info(),
        "foxconn-d41s" => fstart_board_foxconn_d41s::build_info(),
        "foxconn-d41s-uefi" => fstart_board_foxconn_d41s_uefi::build_info(),
        "lenovo-x61" => fstart_board_lenovo_x61::build_info(),
        _ => return None,
    })
}

pub fn board_config_and_driver_bindings(board: &str) -> Option<(BoardConfig, Vec<DriverBinding>)> {
    Some(match board {
        "qemu-riscv64" => (
            fstart_board_qemu_riscv64::board_config(),
            fstart_board_qemu_riscv64::driver_bindings(),
        ),
        "qemu-aarch64" => (
            fstart_board_qemu_aarch64::board_config(),
            fstart_board_qemu_aarch64::driver_bindings(),
        ),
        "foxconn-d41s" => (
            fstart_board_foxconn_d41s::board_config(),
            fstart_board_foxconn_d41s::driver_bindings(),
        ),
        "foxconn-d41s-uefi" => (
            fstart_board_foxconn_d41s_uefi::board_config(),
            fstart_board_foxconn_d41s_uefi::driver_bindings(),
        ),
        "lenovo-x61" => (
            fstart_board_lenovo_x61::board_config(),
            fstart_board_lenovo_x61::driver_bindings(),
        ),
        _ => return None,
    })
}
