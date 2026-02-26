//! Build script for fstart-stage.
//!
//! Reads the board RON file (via FSTART_BOARD_RON env var), then:
//! 1. Generates the stage Rust source (fstart_main + driver init + capabilities)
//! 2. Generates a linker script from the memory map
//! 3. Emits cargo directives for rebuild-on-change

use fstart_codegen::{linker, ron_loader, stage_gen};
use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    // Read the board RON path from environment
    let board_ron_path = env::var("FSTART_BOARD_RON").unwrap_or_else(|_| {
        // Fallback: look for a default board
        panic!(
            "FSTART_BOARD_RON environment variable not set.\n\
             Usage: FSTART_BOARD_RON=boards/qemu-riscv64/board.ron cargo build -p fstart-stage"
        );
    });

    let stage_name = env::var("FSTART_STAGE_NAME").ok();

    println!("cargo:rerun-if-env-changed=FSTART_BOARD_RON");
    println!("cargo:rerun-if-env-changed=FSTART_STAGE_NAME");
    println!("cargo:rerun-if-changed={board_ron_path}");

    // Parse board config (two-phase: typed driver configs + metadata)
    let parsed = ron_loader::load_parsed_board(&PathBuf::from(&board_ron_path))
        .unwrap_or_else(|e| panic!("failed to load board config: {e}"));

    // Generate stage source
    let stage_source = stage_gen::generate_stage_source(&parsed, stage_name.as_deref());
    let stage_path = out_dir.join("generated_stage.rs");
    fs::write(&stage_path, &stage_source).expect("failed to write generated stage");

    // Generate linker script
    let linker_script = linker::generate_linker_script(&parsed.config, stage_name.as_deref());
    let ld_path = out_dir.join("link.ld");
    fs::write(&ld_path, &linker_script).expect("failed to write linker script");

    // Tell cargo to use our generated linker script
    println!("cargo:rustc-link-arg=-T{}", ld_path.display());
}
