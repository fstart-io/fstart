//! Build script for the QEMU Q35 board-owned stage binary.

use std::env;

fn main() {
    println!("cargo:rerun-if-env-changed=FSTART_LINKER_SCRIPT");
    println!("cargo:rerun-if-env-changed=FSTART_SMM_IMAGE");
    println!("cargo:rerun-if-env-changed=FSTART_SMM_COREBOOT_HEADER");

    if let Ok(linker_script) = env::var("FSTART_LINKER_SCRIPT") {
        println!("cargo:rerun-if-changed={linker_script}");
        println!("cargo:rustc-link-arg-bin=fstart-stage=-T{linker_script}");
    }

    if let Ok(smm_image) = env::var("FSTART_SMM_IMAGE") {
        println!("cargo:rerun-if-changed={smm_image}");
        println!("cargo:rustc-env=FSTART_SMM_IMAGE={smm_image}");
    }
    if let Ok(smm_header) = env::var("FSTART_SMM_COREBOOT_HEADER") {
        println!("cargo:rerun-if-changed={smm_header}");
        println!("cargo:rustc-env=FSTART_SMM_COREBOOT_HEADER={smm_header}");
    }
}
