//! Cargo/linker/ELF execution for resolved monolithic XIP stages.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::resolved::ResolvedBuild;

fn cargo_stage(
    root: &Path,
    board: &crate::board_manifest::BoardManifest,
    layout: &ResolvedBuild,
    release: bool,
    checking: bool,
) -> Result<Option<PathBuf>, String> {
    let selection = crate::selection::Selection::prepare(root, board, layout, release)?;
    let directory = &selection.directory;
    let bin = board.stage_bin.as_deref().ok_or("missing stage-bin")?;
    let mut command = selection.command(!checking);
    command.current_dir(root).stderr(Stdio::inherit());
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
            load_addr: layout.code_reservation().base,
        }],
    })
}

pub(crate) fn validate_elf(bytes: &[u8], layout: &ResolvedBuild) -> Result<(), String> {
    fstart_image_build::elf::validate(bytes, &layout.elf_expectations()?)
}
