//! Forwards the SMM image fbuild built for the Zeppelin stage, as
//! `crates/platform-qemu/build.rs` does for q35.

use std::env;

fn main() {
    println!("cargo:rerun-if-env-changed=FSTART_SMM_IMAGE");
    println!("cargo:rustc-check-cfg=cfg(fstart_amd_has_smm_image)");
    if let Ok(smm_image) = env::var("FSTART_SMM_IMAGE") {
        println!("cargo:rerun-if-changed={smm_image}");
        println!("cargo:rustc-env=FSTART_SMM_IMAGE={smm_image}");
        println!("cargo:rustc-cfg=fstart_amd_has_smm_image");
    }
}
