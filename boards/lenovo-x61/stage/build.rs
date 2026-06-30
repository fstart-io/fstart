use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rustc-check-cfg=cfg(fstart_stage_bootblock)");
    println!("cargo:rerun-if-env-changed=FSTART_LINKER_SCRIPT");
    println!("cargo:rerun-if-env-changed=FSTART_STAGE_NAME");
    println!("cargo:rerun-if-env-changed=FSTART_SMM_IMAGE");
    println!("cargo:rerun-if-env-changed=FSTART_SMM_COREBOOT_HEADER");

    if let Ok(script) = env::var("FSTART_LINKER_SCRIPT") {
        println!("cargo:rustc-link-arg-bin=fstart-stage=-T{script}");
        println!("cargo:rerun-if-changed={script}");
    }

    if env::var("FSTART_STAGE_NAME").as_deref() == Ok("bootblock") {
        println!("cargo:rustc-cfg=fstart_stage_bootblock");
    }

    if let Ok(image) = env::var("FSTART_SMM_IMAGE") {
        println!("cargo:rustc-env=FSTART_SMM_IMAGE={image}");
        println!("cargo:rerun-if-changed={image}");
    } else {
        let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR set by Cargo"));
        let empty = out.join("empty-smm.bin");
        fs::write(&empty, []).expect("write empty SMM image placeholder");
        println!("cargo:rustc-env=FSTART_SMM_IMAGE={}", empty.display());
    }

    if let Ok(header) = env::var("FSTART_SMM_COREBOOT_HEADER") {
        println!("cargo:rustc-env=FSTART_SMM_COREBOOT_HEADER={header}");
        println!("cargo:rerun-if-changed={header}");
    }
}
