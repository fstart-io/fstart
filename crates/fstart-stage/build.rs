//! Build script for fstart-stage.
//!
//! For migrated Rust boards, reads board facts directly from the board crate via
//! `FSTART_RUST_BOARD`. For legacy boards, `FSTART_BOARD_RON` remains supported.
//! The generated stage source/linker script are still build artifacts, but board
//! facts are not transported through RON/JSON/postcard for Rust boards.

use fstart_codegen::{linker, ron_loader, stage_gen};
use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let stage_name = env::var("FSTART_STAGE_NAME").ok();

    println!("cargo:rerun-if-env-changed=FSTART_RUST_BOARD");
    println!("cargo:rerun-if-env-changed=FSTART_BOARD_RON");
    println!("cargo:rerun-if-env-changed=FSTART_STAGE_NAME");
    println!("cargo:rerun-if-env-changed=FSTART_SMM_IMAGE");
    println!("cargo:rerun-if-env-changed=FSTART_SMM_COREBOOT_HEADER");
    println!("cargo:rerun-if-env-changed=FSTART_STAGE_ARTIFACT_DIR");
    println!("cargo:rerun-if-env-changed=FSTART_STAGE_FEATURES");

    let (parsed, board_source) = if let Ok(board) = env::var("FSTART_RUST_BOARD") {
        (
            load_rust_board(&board)
                .unwrap_or_else(|e| panic!("failed to load Rust board {board}: {e}")),
            format!("rust:{board}"),
        )
    } else {
        let board_ron_path = env::var("FSTART_BOARD_RON").unwrap_or_else(|_| {
            panic!(
                "neither FSTART_RUST_BOARD nor FSTART_BOARD_RON set; xtask should pass one board source"
            )
        });
        println!("cargo:rerun-if-changed={board_ron_path}");
        (
            ron_loader::load_parsed_board(&PathBuf::from(&board_ron_path))
                .unwrap_or_else(|e| panic!("failed to load legacy board config: {e}")),
            format!("legacy-ron:{board_ron_path}"),
        )
    };

    if let Ok(smm_image) = env::var("FSTART_SMM_IMAGE") {
        println!("cargo:rerun-if-changed={smm_image}");
        println!("cargo:rustc-env=FSTART_SMM_IMAGE={smm_image}");
    }
    if let Ok(smm_header) = env::var("FSTART_SMM_COREBOOT_HEADER") {
        println!("cargo:rerun-if-changed={smm_header}");
        println!("cargo:rustc-env=FSTART_SMM_COREBOOT_HEADER={smm_header}");
    }

    let stage_source = stage_gen::generate_stage_source(&parsed, stage_name.as_deref());
    let stage_path = out_dir.join("generated_stage.rs");
    fs::write(&stage_path, &stage_source).expect("failed to write generated stage");

    let linker_script = linker::generate_linker_script(&parsed, stage_name.as_deref());
    let ld_path = out_dir.join("link.ld");
    fs::write(&ld_path, &linker_script).expect("failed to write linker script");

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
            "board_source={board_source}\nstage={stage_label}\nprofile={profile}\ntarget={target}\nfeatures={features}\nout_dir={}\n",
            out_dir.display()
        );
        fs::write(artifact_dir.join("metadata.txt"), metadata)
            .expect("failed to mirror stage metadata");
        println!(
            "cargo:warning=mirrored generated stage artifacts to {}",
            artifact_dir.display()
        );
    }

    println!("cargo:rustc-link-arg=-T{}", ld_path.display());
}

fn load_rust_board(board: &str) -> Result<ron_loader::ParsedBoard, String> {
    let (config, drivers) = match board {
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
        _ => return Err(format!("unknown Rust board '{board}'")),
    };
    ron_loader::load_parsed_board_from_rust(config, drivers)
}
