//! Build-script shim for fstart-platform-qemu.
//!
//! Forwards the SMM image path fbuild selects for the current stage so the
//! QEMU q35 monolithic stage can embed it (mirrors
//! `crates/platform-intel/build.rs`).

use std::env;

fn main() {
    println!("cargo:rerun-if-env-changed=FSTART_SMM_IMAGE");
    println!("cargo:rustc-check-cfg=cfg(fstart_qemu_has_smm_image)");
    if let Ok(smm_image) = env::var("FSTART_SMM_IMAGE") {
        println!("cargo:rerun-if-changed={smm_image}");
        println!("cargo:rustc-env=FSTART_SMM_IMAGE={smm_image}");
        println!("cargo:rustc-cfg=fstart_qemu_has_smm_image");
    }
}
