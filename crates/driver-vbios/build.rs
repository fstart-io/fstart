fn main() {
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() != Ok("x86_64") {
        return;
    }
    let mut build = cc::Build::new();
    build
        .inherit_rustflags(false)
        .pic(std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("none"));
    build
        .compiler("clang")
        .include("compat")
        .include("vendor/include")
        .include("vendor/x86emu")
        .flag("-include")
        .flag("compat/config.h")
        .flag("-ffreestanding")
        .flag("-fno-stack-protector")
        .flag("-mno-red-zone")
        .flag("-ffunction-sections")
        .flag("-fdata-sections")
        .warnings(false)
        .opt_level(2)
        .file("shim.c");
    for source in ["decode", "fpu", "ops", "ops2", "prim_ops", "sys"] {
        build.file(format!("vendor/x86emu/{source}.c"));
    }
    build.compile("fstart_x86emu");
    println!("cargo:rerun-if-changed=shim.c");
    println!("cargo:rerun-if-changed=compat");
    println!("cargo:rerun-if-changed=vendor");
}
