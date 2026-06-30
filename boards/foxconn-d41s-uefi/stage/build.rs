use std::env;

fn main() {
    println!("cargo:rerun-if-env-changed=FSTART_LINKER_SCRIPT");
    println!("cargo:rerun-if-env-changed=FSTART_STAGE_NAME");
    println!("cargo:rerun-if-env-changed=FSTART_SMM_IMAGE");
    println!("cargo:rerun-if-env-changed=FSTART_SMM_COREBOOT_HEADER");
    println!("cargo:rustc-check-cfg=cfg(fstart_stage_bootblock)");

    if let Ok(linker_script) = env::var("FSTART_LINKER_SCRIPT") {
        println!("cargo:rerun-if-changed={linker_script}");
        println!("cargo:rustc-link-arg-bin=fstart-stage=-T{linker_script}");
    }

    if env::var("FSTART_STAGE_NAME").as_deref() == Ok("bootblock") {
        println!("cargo:rustc-cfg=fstart_stage_bootblock");
    }

    if let Ok(smm_image) = env::var("FSTART_SMM_IMAGE") {
        println!("cargo:rerun-if-changed={smm_image}");
        println!("cargo:rustc-env=FSTART_SMM_IMAGE={smm_image}");
    } else {
        let out_dir = env::var("OUT_DIR").expect("OUT_DIR set by cargo");
        let dummy = std::path::Path::new(&out_dir).join("empty-smm.bin");
        std::fs::write(&dummy, []).expect("write dummy SMM image");
        println!("cargo:rustc-env=FSTART_SMM_IMAGE={}", dummy.display());
    }
    if let Ok(smm_header) = env::var("FSTART_SMM_COREBOOT_HEADER") {
        println!("cargo:rerun-if-changed={smm_header}");
        println!("cargo:rustc-env=FSTART_SMM_COREBOOT_HEADER={smm_header}");
    }
}
