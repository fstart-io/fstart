//! Board discovery from per-board Cargo metadata.
//!
//! Rust-ported boards are normal crates under `boards/` with a small
//! `[package.metadata.fstart]` table. `xtask` discovers those packages and asks
//! their metadata helper binaries to emit board/build facts. RON remains only a
//! temporary transport into the transitional code generator; board files are no
//! longer loaded directly by build orchestration.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use fstart_codegen::ron_loader::{self, ParsedBoard};
use fstart_types::{
    BoardConfig, Build, BuildInfo, BuildProfile, FlowProfile, ImageBuildInfo, PayloadInputInfo,
    SocImageFormat, StageBuildInfo, StageLayout,
};

/// Where a board's authoritative metadata currently lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoardSource {
    /// Normal Rust board crate with `[package.metadata.fstart]`.
    RustCrate,
    /// Legacy `boards/<name>/board.ron` file, not yet ported.
    LegacyRon,
}

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
    /// Metadata source for this board.
    pub source: BoardSource,
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
        } else if dir.join("board.ron").exists() {
            let board = dir
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| format!("invalid board directory name: {}", dir.display()))?
                .to_string();
            boards.push(BoardManifest {
                board,
                package: String::new(),
                dir,
                platform: None,
                target: None,
                source: BoardSource::LegacyRon,
            });
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

    let ron = dir.join("board.ron");
    if ron.exists() {
        return Ok(BoardManifest {
            board: board_name.to_string(),
            package: String::new(),
            dir,
            platform: None,
            target: None,
            source: BoardSource::LegacyRon,
        });
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
        source: BoardSource::RustCrate,
    })
}

/// Load a fully parsed board by asking the Rust board crate for metadata.
pub fn load_parsed_board(workspace_root: &Path, board_name: &str) -> Result<ParsedBoard, String> {
    let manifest = find(workspace_root, board_name)?;
    if manifest.source == BoardSource::LegacyRon {
        return ron_loader::load_parsed_board(&manifest.dir.join("board.ron"));
    }
    let contents = board_metadata(workspace_root, &manifest, "board-config")?;
    ron_loader::load_parsed_board_from_str(&contents, &format!("{} board-config", manifest.package))
}

/// Load only board metadata by asking the Rust board crate for metadata.
pub fn load_board_config(workspace_root: &Path, board_name: &str) -> Result<BoardConfig, String> {
    let parsed = load_parsed_board(workspace_root, board_name)?;
    Ok(parsed.config)
}

/// Load host build/package metadata by asking the Rust board crate.
pub fn load_build_info(workspace_root: &Path, board_name: &str) -> Result<BuildInfo, String> {
    let manifest = find(workspace_root, board_name)?;
    if manifest.source == BoardSource::LegacyRon {
        let parsed = ron_loader::load_parsed_board(&manifest.dir.join("board.ron"))?;
        return legacy_build_info(&manifest, &parsed.config, &parsed.driver_instances);
    }
    let contents = board_metadata(workspace_root, &manifest, "build-info")?;
    let info: BuildInfo = ron::Options::default()
        .from_str(&contents)
        .map_err(|e| format!("failed to parse {} build-info: {e}", manifest.package))?;
    validate_build_info(&manifest, &info)?;
    Ok(info)
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

/// Materialize board metadata into a deterministic file for transitional stage build.rs.
pub fn materialize_board_config(
    workspace_root: &Path,
    manifest: &BoardManifest,
    profile: &str,
    stage_label: &str,
) -> Result<PathBuf, String> {
    if manifest.source == BoardSource::LegacyRon {
        return Ok(manifest.dir.join("board.ron"));
    }
    let contents = board_metadata(workspace_root, manifest, "board-config")?;
    // Validate before handing the file to stage build.rs so helper failures are
    // reported at the xtask layer with the board package name attached.
    ron_loader::load_parsed_board_from_str(
        &contents,
        &format!("{} board-config", manifest.package),
    )?;

    let out_dir = workspace_root
        .join("target")
        .join("rust-board-metadata")
        .join(&manifest.board)
        .join(profile)
        .join(stage_label);
    fs::create_dir_all(&out_dir)
        .map_err(|e| format!("failed to create {}: {e}", out_dir.display()))?;
    let out_path = out_dir.join("board.ron");
    fs::write(&out_path, contents)
        .map_err(|e| format!("failed to write {}: {e}", out_path.display()))?;
    Ok(out_path)
}

fn legacy_build_info(
    manifest: &BoardManifest,
    config: &BoardConfig,
    drivers: &[fstart_device_registry::DriverInstance],
) -> Result<BuildInfo, String> {
    let flow_profile = match &config.stages {
        StageLayout::Monolithic(_) => FlowProfile::LinuxBoot,
        StageLayout::MultiStage(_) => FlowProfile::MultiStage,
    };
    let mut build = Build::new(config.name.as_str())
        .board_package(manifest.package.as_str())
        .target(config.platform.target_triple())
        .profile(BuildProfile::Dev)
        .flow_profile(flow_profile)
        .image(ImageBuildInfo {
            full_flash_image: config.full_flash_image,
            soc_image_format: config.soc_image_format,
        })
        .feature(config.platform.as_str());

    match &config.stages {
        StageLayout::Monolithic(stage) => {
            build = build.stage(StageBuildInfo::new("stage", stage.load_addr));
        }
        StageLayout::MultiStage(stages) => {
            for stage in stages {
                build = build.stage(StageBuildInfo::new(stage.name.as_str(), stage.load_addr));
            }
        }
    }

    for driver in drivers {
        if let Some(feature) = driver.driver_feature() {
            build = build.feature(feature);
        }
    }
    if config.soc_image_format == SocImageFormat::AllwinnerEgon {
        build = build.feature("sunxi");
    }

    if let Some(payload) = &config.payload {
        if let Some(kernel) = &payload.kernel_file {
            build = build.payload_input(PayloadInputInfo::new("kernel", kernel.as_str()));
        }
        if let Some(fit) = &payload.fit_file {
            build = build.payload_input(PayloadInputInfo::new("fit", fit.as_str()));
        }
        if let Some(firmware) = &payload.firmware {
            build = build.payload_input(PayloadInputInfo::new("firmware", firmware.file.as_str()));
        }
    }

    Ok(build.build())
}

fn board_metadata(
    workspace_root: &Path,
    manifest: &BoardManifest,
    command: &str,
) -> Result<String, String> {
    let output = Command::new("cargo")
        .arg("run")
        .arg("--quiet")
        .arg("--package")
        .arg(&manifest.package)
        .arg("--")
        .arg(command)
        .current_dir(workspace_root)
        .output()
        .map_err(|e| {
            format!(
                "failed to run board metadata helper {}: {e}",
                manifest.package
            )
        })?;

    if !output.status.success() {
        return Err(format!(
            "board metadata helper {} {command} failed with status {}\nstdout:\n{}\nstderr:\n{}",
            manifest.package,
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    String::from_utf8(output.stdout).map_err(|e| {
        format!(
            "board metadata helper {} emitted non-UTF-8: {e}",
            manifest.package
        )
    })
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
