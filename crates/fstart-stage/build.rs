//! Build-script shim for fstart-stage.
//!
//! Board-aware build planning lives in xtask. This script only forwards the
//! xtask-produced linker script and a few target environment variables to rustc.

use std::env;

fn main() {
    println!("cargo:rerun-if-env-changed=FSTART_RUST_BOARD");
    println!("cargo:rerun-if-env-changed=FSTART_LINKER_SCRIPT");
    println!("cargo:rerun-if-env-changed=FSTART_STAGE_NAME");
    println!("cargo:rerun-if-env-changed=FSTART_SMM_IMAGE");
    println!("cargo:rerun-if-env-changed=FSTART_SMM_COREBOOT_HEADER");
    println!("cargo:rerun-if-env-changed=FSTART_STAGE_FEATURES");
    println!("cargo:rustc-check-cfg=cfg(fstart_board, values(any()))");

    let board = env::var("FSTART_RUST_BOARD")
        .unwrap_or_else(|_| panic!("FSTART_RUST_BOARD not set; xtask should pass a Rust board"));
    println!("cargo:rustc-cfg=fstart_board=\"{board}\"");

    let linker_script = env::var("FSTART_LINKER_SCRIPT")
        .unwrap_or_else(|_| panic!("FSTART_LINKER_SCRIPT not set; xtask should pass link.ld"));
    println!("cargo:rerun-if-changed={linker_script}");

    if let Ok(smm_image) = env::var("FSTART_SMM_IMAGE") {
        println!("cargo:rerun-if-changed={smm_image}");
        println!("cargo:rustc-env=FSTART_SMM_IMAGE={smm_image}");
    }
    if let Ok(smm_header) = env::var("FSTART_SMM_COREBOOT_HEADER") {
        println!("cargo:rerun-if-changed={smm_header}");
        println!("cargo:rustc-env=FSTART_SMM_COREBOOT_HEADER={smm_header}");
    }
    println!("cargo:rustc-link-arg=-T{linker_script}");
}
