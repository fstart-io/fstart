//! Build-script shim for fstart-stage.
//!
//! Board-aware build planning lives in fbuild and board crates. This script only
//! forwards optional target environment variables for shared stage support code.

fn main() {
    println!(
        "cargo:rustc-check-cfg=cfg(fstart_stage_env, values(\"car\", \"ram\", \"postcar\", \"monolithic\"))"
    );
    println!("cargo:rerun-if-env-changed=FSTART_STAGE_NAME");
    println!("cargo:rerun-if-env-changed=FSTART_STAGE_ENV");
    println!("cargo:rerun-if-env-changed=FSTART_SMM_COREBOOT_HEADER");
    println!("cargo:rerun-if-env-changed=FSTART_STAGE_FEATURES");
    if let Ok(smm_header) = std::env::var("FSTART_SMM_COREBOOT_HEADER") {
        println!("cargo:rerun-if-changed={smm_header}");
        println!("cargo:rustc-env=FSTART_SMM_COREBOOT_HEADER={smm_header}");
    }
}
