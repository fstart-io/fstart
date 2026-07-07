fn main() {
    println!(
        "cargo:rustc-check-cfg=cfg(fstart_stage_env, values(\"car\", \"ram\", \"monolithic\"))"
    );
}
