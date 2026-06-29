//! Direct access to migrated Rust board crates.
//!
//! This is the build-time bridge while boards are still being migrated one by one.
//! It deliberately calls board crate Rust functions directly: no helper binary, no
//! RON/JSON/postcard transport.

use fstart_board_meta::DriverBinding;
use fstart_codegen::board_loader::{load_parsed_board_from_rust_with_acpi, ParsedBoard};
use fstart_types::acpi::AcpiExtraDevice;
use fstart_types::{BoardConfig, BuildInfo};

pub fn parsed_board(board: &str) -> Option<Result<ParsedBoard, String>> {
    let (config, driver_bindings) = board_config_and_driver_bindings(board)?;
    Some(load_parsed_board_from_rust_with_acpi(
        config,
        driver_bindings,
        acpi_only_devices(board),
    ))
}

fn acpi_only_devices(board: &str) -> Vec<AcpiExtraDevice> {
    match board {
        "qemu-sbsa" => fstart_board_qemu_sbsa::acpi_only_devices(),
        _ => Vec::new(),
    }
}

pub fn build_info(board: &str) -> Option<BuildInfo> {
    Some(match board {
        "qemu-riscv64" => fstart_board_qemu_riscv64::build_info(),
        "qemu-riscv64-multi" => fstart_board_qemu_riscv64_multi::build_info(),
        "qemu-aarch64" => fstart_board_qemu_aarch64::build_info(),
        "qemu-aarch64-multi" => fstart_board_qemu_aarch64_multi::build_info(),
        "qemu-aarch64-uefi" => fstart_board_qemu_aarch64_uefi::build_info(),
        "qemu-armv7" => fstart_board_qemu_armv7::build_info(),
        "qemu-sbsa" => fstart_board_qemu_sbsa::build_info(),
        "qemu-q35" => fstart_board_qemu_q35::build_info(),
        "qemu-q35-uefi" => fstart_board_qemu_q35_uefi::build_info(),
        "bananapi-m1" => fstart_board_bananapi_m1::build_info(),
        "orangepi-r1" => fstart_board_orangepi_r1::build_info(),
        "orangepi-pc2" => fstart_board_orangepi_pc2::build_info(),
        "licheerv-dock" => fstart_board_licheerv_dock::build_info(),
        "sifive-unmatched" => fstart_board_sifive_unmatched::build_info(),
        "sifive-unmatched-hw" => fstart_board_sifive_unmatched_hw::build_info(),
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
        "qemu-riscv64-multi" => (
            fstart_board_qemu_riscv64_multi::board_config(),
            fstart_board_qemu_riscv64_multi::driver_bindings(),
        ),
        "qemu-aarch64" => (
            fstart_board_qemu_aarch64::board_config(),
            fstart_board_qemu_aarch64::driver_bindings(),
        ),
        "qemu-aarch64-multi" => (
            fstart_board_qemu_aarch64_multi::board_config(),
            fstart_board_qemu_aarch64_multi::driver_bindings(),
        ),
        "qemu-aarch64-uefi" => (
            fstart_board_qemu_aarch64_uefi::board_config(),
            fstart_board_qemu_aarch64_uefi::driver_bindings(),
        ),
        "qemu-armv7" => (
            fstart_board_qemu_armv7::board_config(),
            fstart_board_qemu_armv7::driver_bindings(),
        ),
        "qemu-sbsa" => (
            fstart_board_qemu_sbsa::board_config(),
            fstart_board_qemu_sbsa::driver_bindings(),
        ),
        "qemu-q35" => (
            fstart_board_qemu_q35::board_config(),
            fstart_board_qemu_q35::driver_bindings(),
        ),
        "qemu-q35-uefi" => (
            fstart_board_qemu_q35_uefi::board_config(),
            fstart_board_qemu_q35_uefi::driver_bindings(),
        ),
        "bananapi-m1" => (
            fstart_board_bananapi_m1::board_config(),
            fstart_board_bananapi_m1::driver_bindings(),
        ),
        "orangepi-r1" => (
            fstart_board_orangepi_r1::board_config(),
            fstart_board_orangepi_r1::driver_bindings(),
        ),
        "orangepi-pc2" => (
            fstart_board_orangepi_pc2::board_config(),
            fstart_board_orangepi_pc2::driver_bindings(),
        ),
        "licheerv-dock" => (
            fstart_board_licheerv_dock::board_config(),
            fstart_board_licheerv_dock::driver_bindings(),
        ),
        "sifive-unmatched" => (
            fstart_board_sifive_unmatched::board_config(),
            fstart_board_sifive_unmatched::driver_bindings(),
        ),
        "sifive-unmatched-hw" => (
            fstart_board_sifive_unmatched_hw::board_config(),
            fstart_board_sifive_unmatched_hw::driver_bindings(),
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
