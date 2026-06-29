use super::*;
use crate::ron_loader::{load_parsed_board_from_rust_with_acpi, ParsedBoard};

/// Load a fixture board, generate the adapter for its first (or
/// only) stage, and return the formatted source.
///
/// Fixture boards are loaded through their direct Rust metadata API.
fn adapter_source_for_board(board: &str) -> String {
    adapter_source_inner(board, None)
}

/// Like [`adapter_source_for_board`] but selects a named stage on
/// multi-stage boards.  Panics if `stage` is not in the board's
/// stage list, mirroring real-build behaviour.
fn adapter_source_for_stage(board: &str, stage: &str) -> String {
    adapter_source_inner(board, Some(stage.to_owned()))
}

/// Runs board loading + codegen on a fresh thread with a generous stack.
fn adapter_source_inner(board: &str, stage: Option<String>) -> String {
    let board = board.to_owned();
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let parsed = load_fixture_board(&board)
                .unwrap_or_else(|e| panic!("failed to load {board}: {e}"));

            // Pick the selected stage, or default to first /
            // monolithic — mirrors `generate_stage_source`.
            let caps: &[Capability] = match (&parsed.config.stages, stage.as_deref()) {
                (fstart_types::StageLayout::Monolithic(m), _) => &m.capabilities,
                (fstart_types::StageLayout::MultiStage(stages), Some(name)) => {
                    &stages
                        .iter()
                        .find(|s| s.name.as_str() == name)
                        .unwrap_or_else(|| panic!("stage {name} not found in board {board}"))
                        .capabilities
                }
                (fstart_types::StageLayout::MultiStage(stages), None) => &stages[0].capabilities,
            };

            let tokens = generate_board_adapter(
                &parsed.config,
                &parsed.driver_instances,
                &parsed.device_tree,
                &parsed.device_services,
                &parsed.acpi_only_devices,
                caps,
                stage.as_deref(),
            );
            let file = syn::parse2::<syn::File>(tokens)
                .unwrap_or_else(|e| panic!("board_gen for {board} produced unparseable Rust: {e}"));
            prettyplease::unparse(&file)
        })
        .expect("spawn codegen worker thread")
        .join()
        .expect("codegen worker thread panicked")
}

fn load_fixture_board(board: &str) -> Result<ParsedBoard, String> {
    let (config, drivers) = match board {
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
        _ => return Err(format!("unknown Rust board fixture '{board}'")),
    };
    let acpi_only_devices = match board {
        "qemu-sbsa" => fstart_board_qemu_sbsa::acpi_only_devices(),
        _ => Vec::new(),
    };
    load_parsed_board_from_rust_with_acpi(config, drivers, acpi_only_devices)
}

mod adapter;
mod logger_fdt;
mod payload_lifecycle_boot;
mod tables_platform_phases;
