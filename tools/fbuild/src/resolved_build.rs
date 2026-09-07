//! Cargo/linker/ELF execution for resolved monolithic XIP stages.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use object::read::elf::{ElfFile64, ProgramHeader};
use object::{Object, ObjectSection, ObjectSymbol};

use crate::resolved::ResolvedBuild;

fn cargo_stage(
    root: &Path,
    board: &crate::board_manifest::BoardManifest,
    layout: &ResolvedBuild,
    release: bool,
    checking: bool,
) -> Result<Option<PathBuf>, String> {
    let directory = layout.artifact_dir(root, &board.board, release)?;
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let script = directory.join("link.ld");
    fs::write(&script, crate::linker::resolved_xip(layout)?).map_err(|e| e.to_string())?;
    fs::write(directory.join("resolved-build.json"), layout.json()?).map_err(|e| e.to_string())?;
    // load() already prepared this workspace and resolved its dependencies.
    let workspace = root.join("target/fstart-workspaces").join(&board.board);
    let bin = board.stage_bin.as_deref().ok_or("missing stage-bin")?;
    let payload = if layout.payload == "uefi" {
        "crabefi"
    } else {
        &layout.payload
    };
    let mut flags = crate::toolchain::rustflags_for_triple(&layout.target);
    flags.push_str(&format!(
        " --cfg fstart_stage_env=\"{}\" --cfg fstart_entry=\"{}\" --cfg fstart_payload=\"{payload}\" --check-cfg=cfg(fstart_stage_env,values(\"monolithic\")) --check-cfg=cfg(fstart_entry,values(\"riscv64\")) --check-cfg=cfg(fstart_payload,values(\"halt\",\"linux\",\"crabefi\")) -Clink-arg=-T{}",
        layout.env, layout.entry, script.display(),
    ));
    if std::env::var_os("FSTART_EXTRA_RUSTFLAGS").is_some() {
        return Err("resolved builds do not accept unrecorded FSTART_EXTRA_RUSTFLAGS".into());
    }
    fs::write(directory.join("rustflags.txt"), &flags).map_err(|e| e.to_string())?;
    let mut command = Command::new("cargo");
    command
        .current_dir(root)
        .args([
            if checking { "check" } else { "build" },
            "--locked",
            "--message-format=json-render-diagnostics",
            "--no-default-features",
        ])
        .arg("--manifest-path")
        .arg(workspace.join("Cargo.toml"))
        .arg("--package")
        .arg(&board.package)
        .arg("--bin")
        .arg(bin)
        .arg("--target")
        .arg(&layout.target)
        .arg("--target-dir")
        .arg(directory.join("cargo"))
        .arg("--features")
        .arg(layout.features.join(","))
        .args(["-Z", "build-std=core,alloc"])
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env("RUSTFLAGS", flags)
        .stderr(Stdio::inherit());
    if release {
        command.arg("--release");
    }
    eprintln!(
        "[fstart] resolved build: {} ({}, {})",
        board.board, layout.target, layout.payload
    );
    eprintln!("[fstart] build artifacts: {}", directory.display());
    let result = command.output().map_err(|e| format!("cargo build: {e}"))?;
    if !result.status.success() {
        return Err("resolved stage build failed".into());
    }
    let mut artifacts = Vec::new();
    for line in result
        .stdout
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
    {
        let value: serde_json::Value =
            serde_json::from_slice(line).map_err(|e| format!("Cargo artifact JSON: {e}"))?;
        if value["reason"] == "compiler-artifact"
            && value["target"]["name"] == bin
            && let Some(path) = value["executable"].as_str()
        {
            artifacts.push(std::path::PathBuf::from(path));
        }
    }
    if checking {
        return Ok(None);
    }
    if artifacts.len() != 1 {
        return Err(format!(
            "expected one stage executable, got {}",
            artifacts.len()
        ));
    }
    Ok(artifacts.pop())
}

pub fn check(
    root: &Path,
    board: &crate::board_manifest::BoardManifest,
    layout: &ResolvedBuild,
    release: bool,
) -> Result<(), String> {
    cargo_stage(root, board, layout, release, true).map(|_| ())
}

pub fn build(
    root: &Path,
    board: &crate::board_manifest::BoardManifest,
    layout: &ResolvedBuild,
    release: bool,
) -> Result<crate::build_board::BuildResult, String> {
    let elf = cargo_stage(root, board, layout, release, false)?.ok_or("missing built stage")?;
    let directory = layout.artifact_dir(root, &board.board, release)?;
    validate_elf(&fs::read(&elf).map_err(|e| e.to_string())?, layout)?;
    let flat = directory.join("stage.bin");
    crate::build_board::write_flat_binary(&elf, &flat)?;
    if fs::metadata(&flat).map_err(|e| e.to_string())?.len() > layout.image.size {
        return Err("flat stage exceeds image capacity".into());
    }
    eprintln!("[fstart] built: {}", flat.display());
    Ok(crate::build_board::BuildResult {
        stages: vec![fstart_image_build::StageBinary {
            name: "stage".into(),
            path: elf,
            run_path: flat,
            load_addr: layout.image.base,
        }],
    })
}

pub(crate) fn validate_elf(bytes: &[u8], layout: &ResolvedBuild) -> Result<(), String> {
    let elf = ElfFile64::<object::Endianness>::parse(bytes).map_err(|e| e.to_string())?;
    for segment in elf
        .elf_program_headers()
        .iter()
        .filter(|s| s.p_type(elf.endian()) == object::elf::PT_LOAD)
    {
        let paddr = segment.p_paddr(elf.endian());
        let vaddr = segment.p_vaddr(elf.endian());
        let filesz = segment.p_filesz(elf.endian());
        let memsz = segment.p_memsz(elf.endian());
        if filesz > memsz
            || (filesz != 0 && !layout.image.contains(paddr, filesz))
            || (memsz != 0
                && !layout.image.contains(vaddr, memsz)
                && !layout.writable.contains(vaddr, memsz))
        {
            return Err(format!(
                "PT_LOAD exceeds resolved reservations: physical {paddr:#x}, virtual {vaddr:#x}, file {filesz:#x}, memory {memsz:#x}"
            ));
        }
    }
    let object = object::File::parse(bytes).map_err(|e| e.to_string())?;
    let section = object
        .section_by_name(".fstart.layout")
        .ok_or("ELF lost layout descriptor")?;
    if section.data().map_err(|e| e.to_string())? != layout.descriptor()?.as_bytes() {
        return Err("ELF descriptor differs from resolved build".into());
    }
    if !layout.image.contains(section.address(), section.size()) {
        return Err("descriptor outside image reservation".into());
    }
    let object::SectionFlags::Elf { sh_flags } = section.flags() else {
        return Err("descriptor is not an ELF section".into());
    };
    if section.address() % 8 != 0
        || section.file_range().is_none()
        || sh_flags & u64::from(object::elf::SHF_ALLOC) == 0
        || sh_flags & u64::from(object::elf::SHF_WRITE) != 0
    {
        return Err("descriptor is not aligned, loaded read-only storage".into());
    }
    for (symbol, address) in [
        ("_fstart_layout_start", section.address()),
        ("_fstart_layout_end", section.address() + section.size()),
        ("_stack_bottom", layout.stack.base),
        ("_stack_top", layout.stack.base + layout.stack.size),
        ("_FSTART_HEAP", layout.heap.base),
    ] {
        if !object
            .symbols()
            .any(|s| s.name() == Ok(symbol) && s.address() == address)
        {
            return Err(format!("ELF {symbol} differs from resolved reservation"));
        }
    }
    Ok(())
}
