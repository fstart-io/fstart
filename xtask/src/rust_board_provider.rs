//! Direct access to migrated Rust board crates.
//!
//! This is the build-time bridge while boards are still being migrated one by one.
//! It deliberately calls board crate Rust functions directly: no helper binary, no
//! RON/JSON/postcard transport.

use fstart_codegen::ron_loader::{load_parsed_board_from_rust, ParsedBoard};
use fstart_device_registry::DriverInstance;
use fstart_types::{BoardConfig, BuildInfo};

pub fn parsed_board(board: &str) -> Option<Result<ParsedBoard, String>> {
    let (config, drivers) = board_config_and_drivers(board)?;
    Some(load_parsed_board_from_rust(config, drivers))
}

pub fn build_info(board: &str) -> Option<BuildInfo> {
    Some(match board {
        "qemu-riscv64" => fstart_board_qemu_riscv64::build_info(),
        "qemu-aarch64" => fstart_board_qemu_aarch64::build_info(),
        "foxconn-d41s" => fstart_board_foxconn_d41s::build_info(),
        "foxconn-d41s-uefi" => fstart_board_foxconn_d41s_uefi::build_info(),
        _ => return None,
    })
}

pub fn board_config_and_drivers(board: &str) -> Option<(BoardConfig, Vec<DriverInstance>)> {
    Some(match board {
        "qemu-riscv64" => (
            fstart_board_qemu_riscv64::board_config(),
            fstart_board_qemu_riscv64::driver_instances(),
        ),
        "qemu-aarch64" => (
            fstart_board_qemu_aarch64::board_config(),
            fstart_board_qemu_aarch64::driver_instances(),
        ),
        "foxconn-d41s" => (
            fstart_board_foxconn_d41s::board_config(),
            fstart_board_foxconn_d41s::driver_instances(),
        ),
        "foxconn-d41s-uefi" => (
            fstart_board_foxconn_d41s_uefi::board_config(),
            fstart_board_foxconn_d41s_uefi::driver_instances(),
        ),
        _ => return None,
    })
}
