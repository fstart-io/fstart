use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rustc-check-cfg=cfg(rust_analyzer)");
    println!("cargo:rerun-if-changed=asm/sipi_trampoline.S");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR set by Cargo"));
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let source = manifest_dir.join("asm/sipi_trampoline.S");

    let object = out_dir.join("sipi_trampoline.o");
    let elf = out_dir.join("sipi_trampoline.elf");
    let bin = out_dir.join("sipi_trampoline.bin");

    run(Command::new("cc")
        .arg("-c")
        .arg("-x")
        .arg("assembler-with-cpp")
        .arg(&source)
        .arg("-o")
        .arg(&object));
    run(Command::new("ld")
        .arg("-nostdlib")
        .arg("-Ttext=0")
        .arg("--oformat=elf64-x86-64")
        .arg("-o")
        .arg(&elf)
        .arg(&object));
    run(Command::new("objcopy")
        .arg("-O")
        .arg("binary")
        .arg("-j")
        .arg(".text")
        .arg(&elf)
        .arg(&bin));

    fs::write(
        out_dir.join("sipi_trampoline.rs"),
        format!(
            "pub const TRAMPOLINE: &[u8] = include_bytes!(r#\"{}\"#);\n\
             pub const CR3_OFFSET: usize = {:#x};\n\
             pub const ENTRY_OFFSET: usize = {:#x};\n\
             pub const STACK_BASE_OFFSET: usize = {:#x};\n\
             pub const STACK_SIZE_OFFSET: usize = {:#x};\n\
             pub const AP_COUNTER_OFFSET: usize = {:#x};\n",
            bin.display(),
            find_symbol_offset(elf.as_path(), "fstart_sipi_cr3"),
            find_symbol_offset(elf.as_path(), "fstart_sipi_entry"),
            find_symbol_offset(elf.as_path(), "fstart_sipi_stack_base"),
            find_symbol_offset(elf.as_path(), "fstart_sipi_stack_size"),
            find_symbol_offset(elf.as_path(), "fstart_sipi_ap_counter"),
        ),
    )
    .unwrap();
}

fn find_symbol_offset(elf: &Path, symbol: &str) -> usize {
    let output = Command::new("nm")
        .arg("--defined-only")
        .arg("--numeric-sort")
        .arg(elf)
        .output()
        .unwrap_or_else(|e| panic!("failed to run nm on {}: {e}", elf.display()));
    if !output.status.success() {
        panic!(
            "nm failed for {}:\n{}",
            elf.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8(output.stdout).unwrap();
    for line in stdout.lines() {
        let mut parts = line.split_whitespace();
        let addr = match parts.next() {
            Some(addr) => addr,
            None => continue,
        };
        let _kind = parts.next();
        let name = match parts.next() {
            Some(name) => name,
            None => continue,
        };
        if name == symbol {
            return usize::from_str_radix(addr, 16)
                .unwrap_or_else(|e| panic!("bad nm address for {symbol}: {e}"));
        }
    }
    panic!("symbol {symbol} not found in {}", elf.display());
}

fn run(cmd: &mut Command) {
    let output = cmd
        .output()
        .unwrap_or_else(|e| panic!("failed to run {:?}: {e}", cmd));
    if !output.status.success() {
        panic!(
            "command failed: {:?}\nstdout:\n{}\nstderr:\n{}",
            cmd,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}
