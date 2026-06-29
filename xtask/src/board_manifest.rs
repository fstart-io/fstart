//! Board discovery from per-board Cargo metadata.
//!
//! Boards are normal crates under `boards/` with a small
//! `[package.metadata.fstart]` table. `xtask` discovers those packages and loads
//! board/build facts by running each board crate's metadata binary.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use fstart_board_meta::HostBoardMetadata;
use fstart_codegen::board_loader::ParsedBoard;
use fstart_types::{BoardConfig, BuildInfo};

/// Discovery metadata for one board crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardManifest {
    /// Stable fstart board name.
    pub board: String,
    /// Cargo package name for the board crate.
    pub package: String,
    /// Board crate directory.
    pub dir: PathBuf,
    /// Platform metadata string from the board crate.
    pub platform: Option<String>,
    /// Rust target triple metadata string from the board crate.
    pub target: Option<String>,
    /// Cargo binary that emits host board metadata as JSON.
    pub metadata_bin: String,
    /// Optional Cargo binary that owns this board's static stage adapter.
    pub stage_bin: Option<String>,
}

/// Discover every board crate below `boards/`.
#[allow(dead_code)]
pub fn discover(workspace_root: &Path) -> Result<Vec<BoardManifest>, String> {
    let boards_dir = workspace_root.join("boards");
    let mut boards = Vec::new();

    for entry in fs::read_dir(&boards_dir)
        .map_err(|e| format!("failed to read {}: {e}", boards_dir.display()))?
    {
        let entry = entry.map_err(|e| format!("failed to read board dir entry: {e}"))?;
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let manifest = dir.join("Cargo.toml");
        if manifest.exists() {
            boards.push(read(&manifest)?);
        }
    }

    boards.sort_by(|a, b| a.board.cmp(&b.board));
    Ok(boards)
}

/// Find one board by its stable fstart name.
pub fn find(workspace_root: &Path, board_name: &str) -> Result<BoardManifest, String> {
    let dir = workspace_root.join("boards").join(board_name);
    let manifest = dir.join("Cargo.toml");
    if manifest.exists() {
        let board = read(&manifest)?;
        if board.board != board_name {
            return Err(format!(
                "board metadata mismatch in {}: expected '{}', found '{}'",
                manifest.display(),
                board_name,
                board.board
            ));
        }
        return Ok(board);
    }

    Err(format!(
        "board '{board_name}' not found in boards/{board_name}/Cargo.toml metadata"
    ))
}

fn read(manifest: &Path) -> Result<BoardManifest, String> {
    let text = fs::read_to_string(manifest)
        .map_err(|e| format!("failed to read {}: {e}", manifest.display()))?;
    let dir = manifest
        .parent()
        .ok_or_else(|| format!("manifest has no parent: {}", manifest.display()))?
        .to_path_buf();

    let package = package_name(&text).ok_or_else(|| {
        format!(
            "missing [package].name in board manifest {}",
            manifest.display()
        )
    })?;
    let board = metadata_value(&text, "board")
        .or_else(|| {
            dir.file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
        })
        .ok_or_else(|| {
            format!(
                "missing package.metadata.fstart.board in {}",
                manifest.display()
            )
        })?;

    Ok(BoardManifest {
        board,
        package,
        dir,
        platform: metadata_value(&text, "platform"),
        target: metadata_value(&text, "target"),
        metadata_bin: metadata_value(&text, "metadata-bin")
            .unwrap_or_else(|| "fstart-board-metadata".to_string()),
        stage_bin: metadata_value(&text, "stage-bin"),
    })
}

/// Load a fully parsed board by asking the Rust board crate for metadata.
pub fn load_parsed_board(workspace_root: &Path, board_name: &str) -> Result<ParsedBoard, String> {
    let metadata = load_host_metadata(workspace_root, board_name)?;
    fstart_codegen::board_loader::load_parsed_board_from_metadata(metadata)
}

/// Load only board metadata by asking the Rust board crate for metadata.
pub fn load_board_config(workspace_root: &Path, board_name: &str) -> Result<BoardConfig, String> {
    let parsed = load_parsed_board(workspace_root, board_name)?;
    Ok(parsed.config)
}

/// Load host build/package metadata by asking the Rust board crate.
pub fn load_build_info(workspace_root: &Path, board_name: &str) -> Result<BuildInfo, String> {
    let manifest = find(workspace_root, board_name)?;
    let info = load_host_metadata_from_manifest(workspace_root, &manifest)?.build_info;
    validate_build_info(&manifest, &info)?;
    Ok(info)
}

/// Load host board metadata by running the discovered board metadata binary.
pub fn load_host_metadata(
    workspace_root: &Path,
    board_name: &str,
) -> Result<HostBoardMetadata, String> {
    let manifest = find(workspace_root, board_name)?;
    let metadata = load_host_metadata_from_manifest(workspace_root, &manifest)?;
    validate_build_info(&manifest, &metadata.build_info)?;
    Ok(metadata)
}

fn load_host_metadata_from_manifest(
    workspace_root: &Path,
    manifest: &BoardManifest,
) -> Result<HostBoardMetadata, String> {
    let output = Command::new("cargo")
        .current_dir(workspace_root)
        .arg("run")
        .arg("--quiet")
        .arg("--package")
        .arg(&manifest.package)
        .arg("--bin")
        .arg(&manifest.metadata_bin)
        .output()
        .map_err(|e| {
            format!(
                "failed to run board metadata binary for {}: {e}",
                manifest.board
            )
        })?;

    if !output.status.success() {
        return Err(format!(
            "board metadata binary '{}' for '{}' failed with {}\n{}",
            manifest.metadata_bin,
            manifest.board,
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    serde_json::from_slice(&output.stdout).map_err(|e| {
        format!(
            "failed to parse board metadata JSON from {}:{}: {e}\nstdout:\n{}\nstderr:\n{}",
            manifest.package,
            manifest.metadata_bin,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn validate_build_info(manifest: &BoardManifest, info: &BuildInfo) -> Result<(), String> {
    if info.name.as_str() != manifest.board {
        return Err(format!(
            "build_info name mismatch for {}: manifest board is '{}', helper returned '{}'",
            manifest.package, manifest.board, info.name
        ));
    }
    if info.board_package.as_str() != manifest.package {
        return Err(format!(
            "build_info package mismatch for {}: helper returned '{}'",
            manifest.package, info.board_package
        ));
    }
    if let Some(target) = &manifest.target {
        if info.target.as_str() != target {
            return Err(format!(
                "build_info target mismatch for {}: manifest target is '{}', helper returned '{}'",
                manifest.package, target, info.target
            ));
        }
    }
    Ok(())
}

fn package_name(text: &str) -> Option<String> {
    let mut in_package = false;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_package = trimmed == "[package]";
            continue;
        }
        if !in_package || trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        let Some((found_key, value)) = trimmed.split_once('=') else {
            continue;
        };
        if found_key.trim() == "name" {
            return Some(value.trim().trim_matches('"').to_string());
        }
    }

    None
}

fn metadata_value(text: &str, key: &str) -> Option<String> {
    let mut in_fstart_metadata = false;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_fstart_metadata = trimmed == "[package.metadata.fstart]";
            continue;
        }
        if !in_fstart_metadata || trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        let Some((found_key, value)) = trimmed.split_once('=') else {
            continue;
        };
        if found_key.trim() != key {
            continue;
        }
        let value = value.trim().trim_matches('"');
        return Some(value.to_string());
    }

    None
}

#[cfg(test)]
mod tests {
    use super::metadata_value;

    #[test]
    fn reads_only_fstart_metadata_values() {
        let text = r#"
            [package]
            name = "not-a-board"

            [package.metadata.fstart]
            board = "qemu-riscv64"
            target = "riscv64gc-unknown-none-elf"
        "#;

        assert_eq!(
            metadata_value(text, "board").as_deref(),
            Some("qemu-riscv64")
        );
        assert_eq!(
            metadata_value(text, "target").as_deref(),
            Some("riscv64gc-unknown-none-elf")
        );
        assert_eq!(metadata_value(text, "name"), None);
    }
}
