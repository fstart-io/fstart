use std::env;

fn main() {
    println!("cargo:rustc-check-cfg=cfg(smm_platform, values(\"lenovo-x61\"))");
    println!("cargo:rerun-if-env-changed=FSTART_SMM_PLATFORM");

    let platform = env::var("FSTART_SMM_PLATFORM").unwrap_or_else(|_| "lenovo-x61".to_string());
    match platform.as_str() {
        "lenovo-x61" => {
            println!("cargo:rustc-cfg=smm_platform=\"{platform}\"");
        }
        other => panic!("unsupported FSTART_SMM_PLATFORM={other}"),
    }
}
