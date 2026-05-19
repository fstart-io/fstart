use std::env;

fn main() {
    println!("cargo:rustc-check-cfg=cfg(smm_platform, values(\"qemu-q35\", \"pineview-ich7\", \"lenovo-x61\"))");
    println!("cargo:rerun-if-env-changed=FSTART_SMM_PLATFORM");

    let platform = env::var("FSTART_SMM_PLATFORM").unwrap_or_else(|_| "pineview-ich7".to_string());
    match platform.as_str() {
        "qemu-q35" | "pineview-ich7" | "lenovo-x61" => {
            println!("cargo:rustc-cfg=smm_platform=\"{platform}\"");
        }
        other => panic!("unsupported FSTART_SMM_PLATFORM={other}"),
    }
}
