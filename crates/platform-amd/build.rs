//! Forwards the SMM image fbuild built for the Zeppelin stage, as
//! `crates/platform-qemu/build.rs` does for q35.

use std::env;

fn main() {
    println!("cargo:rerun-if-env-changed=FSTART_RS_MICROCODE");
    println!("cargo:rustc-check-cfg=cfg(fstart_amd_has_rs_microcode)");
    if let Ok(patch) = env::var("FSTART_RS_MICROCODE") {
        println!("cargo:rerun-if-changed={patch}");
        println!("cargo:rustc-env=FSTART_RS_MICROCODE={patch}");
        println!("cargo:rustc-cfg=fstart_amd_has_rs_microcode");
    }
    println!("cargo:rerun-if-env-changed=FSTART_RS_LINUX");
    println!("cargo:rustc-check-cfg=cfg(fstart_amd_has_rs_linux)");
    if let Ok(kernel) = env::var("FSTART_RS_LINUX") {
        println!("cargo:rerun-if-changed={kernel}");
        println!("cargo:rustc-env=FSTART_RS_LINUX={kernel}");
        println!("cargo:rustc-cfg=fstart_amd_has_rs_linux");
    }
    for key in ["FSTART_RS_LINUX_ARGS", "FSTART_RS_LINUX_MTRRS"] {
        println!("cargo:rerun-if-env-changed={key}");
        if let Ok(value) = env::var(key) {
            println!("cargo:rustc-env={key}={value}");
        }
    }
    println!("cargo:rerun-if-env-changed=FSTART_SMM_IMAGE");
    println!("cargo:rustc-check-cfg=cfg(fstart_amd_has_smm_image)");
    if let Ok(smm_image) = env::var("FSTART_SMM_IMAGE") {
        println!("cargo:rerun-if-changed={smm_image}");
        println!("cargo:rustc-env=FSTART_SMM_IMAGE={smm_image}");
        println!("cargo:rustc-cfg=fstart_amd_has_smm_image");
    }
}
