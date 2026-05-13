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
    println!("cargo:rerun-if-env-changed=FSTART_SMM_IMAGE");
    println!("cargo:rerun-if-env-changed=FSTART_SMM_COREBOOT_HEADER");
    println!("cargo:rerun-if-env-changed=FSTART_STAGE_ARTIFACT_DIR");
    println!("cargo:rerun-if-env-changed=FSTART_STAGE_FEATURES");
    println!("cargo:rerun-if-changed={board_ron_path}");
    if let Ok(smm_image) = env::var("FSTART_SMM_IMAGE") {
        println!("cargo:rerun-if-changed={smm_image}");
        println!("cargo:rustc-env=FSTART_SMM_IMAGE={smm_image}");
    }
    if let Ok(smm_header) = env::var("FSTART_SMM_COREBOOT_HEADER") {
        println!("cargo:rerun-if-changed={smm_header}");
        println!("cargo:rustc-env=FSTART_SMM_COREBOOT_HEADER={smm_header}");
    }

    // Parse board config (two-phase: typed driver configs + metadata)
    let parsed = ron_loader::load_parsed_board(&PathBuf::from(&board_ron_path))
        .unwrap_or_else(|e| panic!("failed to load board config: {e}"));

    // Generate stage source.
    let stage_source = stage_gen::generate_stage_source(&parsed, stage_name.as_deref());
    let stage_path = out_dir.join("generated_stage.rs");
    fs::write(&stage_path, &stage_source).expect("failed to write generated stage");

    // Generate linker script.
    let linker_script = linker::generate_linker_script(&parsed.config, stage_name.as_deref());
    let ld_path = out_dir.join("link.ld");
    fs::write(&ld_path, &linker_script).expect("failed to write linker script");

    // Cargo's OUT_DIR is intentionally opaque and can be hard to map back
    // to a logical firmware stage.  xtask passes a deterministic mirror
    // directory so humans can inspect generated code without hunting through
    // target/.../build/fstart-stage-*/out or guessing which hash is which.
    if let Ok(artifact_dir) = env::var("FSTART_STAGE_ARTIFACT_DIR") {
        let artifact_dir = PathBuf::from(artifact_dir);
        fs::create_dir_all(&artifact_dir).expect("failed to create stage artifact dir");
        fs::write(artifact_dir.join("generated_stage.rs"), &stage_source)
            .expect("failed to mirror generated stage");
        fs::write(artifact_dir.join("link.ld"), &linker_script)
            .expect("failed to mirror linker script");

        let stage_label = stage_name.as_deref().unwrap_or("stage");
        let features = env::var("FSTART_STAGE_FEATURES").unwrap_or_default();
        let profile = env::var("PROFILE").unwrap_or_default();
        let target = env::var("TARGET").unwrap_or_default();
        let metadata = format!(
            "board_ron={board_ron_path}\nstage={stage_label}\nprofile={profile}\ntarget={target}\nfeatures={features}\nout_dir={}\n",
            out_dir.display()
        );
        fs::write(artifact_dir.join("metadata.txt"), metadata)
            .expect("failed to mirror stage metadata");
        println!(
            "cargo:warning=mirrored generated stage artifacts to {}",
            artifact_dir.display()
        );
    }

    // Tell cargo to use our generated linker script.
    println!("cargo:rustc-link-arg=-T{}", ld_path.display());
}
