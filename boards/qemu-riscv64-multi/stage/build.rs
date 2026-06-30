fn main() {
    println!("cargo:rerun-if-env-changed=FSTART_LINKER_SCRIPT");
    println!("cargo:rerun-if-env-changed=FSTART_STAGE_NAME");
    println!("cargo:rustc-check-cfg=cfg(fstart_stage_bootblock)");

    if let Ok(linker_script) = std::env::var("FSTART_LINKER_SCRIPT") {
        println!("cargo:rerun-if-changed={linker_script}");
        println!("cargo:rustc-link-arg-bin=fstart-stage=-T{linker_script}");
    }

    if std::env::var("FSTART_STAGE_NAME").as_deref() == Ok("bootblock") {
        println!("cargo:rustc-cfg=fstart_stage_bootblock");
    }
}
