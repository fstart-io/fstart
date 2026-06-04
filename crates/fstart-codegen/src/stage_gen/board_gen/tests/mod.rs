use super::*;
use crate::ron_loader::load_parsed_board;
use std::path::PathBuf;

/// Load a fixture board, generate the adapter for its first (or
/// only) stage, and return the formatted source.
///
/// Matches the path resolution `tests.rs` already uses — look up
/// `boards/<name>/board.ron` relative to the workspace root.
fn adapter_source_for_board(board: &str) -> String {
    adapter_source_inner(board, None)
}

/// Like [`adapter_source_for_board`] but selects a named stage on
/// multi-stage boards.  Panics if `stage` is not in the board's
/// stage list, mirroring real-build behaviour.
fn adapter_source_for_stage(board: &str, stage: &str) -> String {
    adapter_source_inner(board, Some(stage.to_owned()))
}

/// Runs the ron loader + codegen on a fresh thread with a
/// generous stack (8 MiB).  The Rust default test-thread stack
/// is 2 MiB and `prettyplease` + serde-de-deep-ron can exceed that
/// for some boards when compiled in debug mode.  Using a worker
/// thread keeps every test robust without forcing every CI run
/// to export `RUST_MIN_STACK`.
fn adapter_source_inner(board: &str, stage: Option<String>) -> String {
    let board = board.to_owned();
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .to_path_buf();
            let ron = root.join("boards").join(&board).join("board.ron");
            let parsed =
                load_parsed_board(&ron).unwrap_or_else(|e| panic!("failed to load {board}: {e}"));

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

mod adapter;
mod logger_fdt;
mod payload_lifecycle_boot;
mod tables_platform_phases;
