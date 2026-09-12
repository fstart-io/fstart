use object::{Object, ObjectSection};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rustc-check-cfg=cfg(rust_analyzer)");
    println!("cargo:rerun-if-changed=asm/entry_stub.S");
    println!("cargo:rerun-if-changed=../smm/src/runtime_abi.rs");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"));
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());

    let entry = build_asm_blob(
        &manifest_dir.join("asm/entry_stub.S"),
        &out_dir,
        "entry_stub",
        "fstart_smm_stub_params",
    );

    fs::write(
        out_dir.join("smm_image_asm.rs"),
        format!(
            "pub const ENTRY_STUB: &[u8] = include_bytes!(r#\"{}\"#);\n\
             pub const ENTRY_PARAMS_OFFSET: usize = {:#x};\n",
            entry.bin.display(),
            entry.symbol_offset,
        ),
    )
    .unwrap();
}

struct BuiltBlob {
    bin: PathBuf,
    symbol_offset: usize,
}

fn build_asm_blob(source: &Path, out_dir: &Path, stem: &str, symbol: &str) -> BuiltBlob {
    let object = out_dir.join(format!("{stem}.o"));
    let elf = out_dir.join(format!("{stem}.elf"));
    let bin = out_dir.join(format!("{stem}.bin"));

    run(Command::new("cc")
        .arg("-c")
        .arg("-x")
        .arg("assembler-with-cpp")
        .arg(source)
        .arg("-o")
        .arg(&object));
    link_text_blob(object.as_path(), elf.as_path(), bin.as_path());

    let symbol_offset = find_symbol_offset(elf.as_path(), symbol);
    BuiltBlob { bin, symbol_offset }
}

fn link_text_blob(object: &Path, elf: &Path, bin: &Path) {
    run(Command::new("ld")
        .arg("-nostdlib")
        .arg("-Ttext=0")
        .arg("--oformat=elf64-x86-64")
        .arg("-o")
        .arg(elf)
        .arg(object));
    write_text_section(elf, bin);
}

fn write_text_section(elf: &Path, bin: &Path) {
    let data = fs::read(elf).unwrap_or_else(|e| panic!("failed to read {}: {e}", elf.display()));
    let file = object::File::parse(data.as_slice())
        .unwrap_or_else(|e| panic!("failed to parse ELF {}: {e}", elf.display()));
    let section = file
        .section_by_name(".text")
        .unwrap_or_else(|| panic!(".text section not found in {}", elf.display()));
    let text = section
        .data()
        .unwrap_or_else(|e| panic!("failed to read .text from {}: {e}", elf.display()));
    fs::write(bin, text).unwrap_or_else(|e| panic!("failed to write {}: {e}", bin.display()));
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
        let parts: Vec<_> = line.split_whitespace().collect();
        if parts.len() >= 3 && parts[2] == symbol {
            return usize::from_str_radix(parts[0], 16)
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
