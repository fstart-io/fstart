//! Build-script shim for fstart-platform-sp3: declares the cfgs fbuild sets
//! for the reset unit.

fn main() {
    println!("cargo:rustc-check-cfg=cfg(fstart_entry, values(\"amd-psp-reset\"))");
    println!("cargo:rustc-check-cfg=cfg(fstart_stage_env, values(\"ram\", \"smm\"))");
}
