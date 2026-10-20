//! Build-script shim for fstart-platform-sp3: declares the cfgs fbuild sets
//! for the reset unit and forwards the SMM handler image when one is built.

fn main() {
    println!("cargo:rustc-check-cfg=cfg(fstart_entry, values(\"amd-psp-reset\"))");
    println!(
        "cargo:rustc-check-cfg=cfg(fstart_stage_env, values(\"monolithic\", \"ram\", \"smm\"))"
    );
    println!(
        "cargo:rustc-check-cfg=cfg(fstart_payload, values(\"halt\", \"linux\", \"crabefi\", \"coreboot\"))"
    );
    println!("cargo:rustc-check-cfg=cfg(fstart_sp3_has_smm_image)");
    println!("cargo:rerun-if-env-changed=FSTART_SMM_IMAGE");
    if let Ok(image) = std::env::var("FSTART_SMM_IMAGE") {
        println!("cargo:rerun-if-changed={image}");
        println!("cargo:rustc-env=FSTART_SMM_IMAGE={image}");
        println!("cargo:rustc-cfg=fstart_sp3_has_smm_image");
    }
}
