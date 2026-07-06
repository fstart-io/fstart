//! Board discovery from per-board Cargo metadata.
//!
//! Boards are normal crates under `boards/` with a small
//! `[package.metadata.fstart]` table. `xtask` discovers those packages from
//! Cargo metadata; stage builds use the board-owned package manifest directly.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

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
    /// Board-selected Cargo/backend features.
    pub features: Vec<String>,
    /// Whether this board exports ACPI-only device metadata.
    pub acpi_only_devices: bool,
    /// Whether this board has a host feature for host-only dependencies.
    pub host_feature: bool,
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
        features: metadata_list(&text, "features"),
        acpi_only_devices: metadata_bool(&text, "acpi-only-devices").unwrap_or(false),
        host_feature: has_feature(&text, "host"),
        stage_bin: metadata_value(&text, "stage-bin"),
    })
}

/// Dispatch an xtask subcommand to a generated host tool for the selected board.
pub fn run_board_tool(
    workspace_root: &Path,
    board_name: &str,
    args: &[String],
) -> Result<(), String> {
    let manifest = find(workspace_root, board_name)?;
    let tool_dir = workspace_root
        .join("target")
        .join("fstart-board-tools")
        .join(&manifest.board);
    let src_dir = tool_dir.join("src");
    fs::create_dir_all(&src_dir)
        .map_err(|e| format!("failed to create {}: {e}", src_dir.display()))?;

    let board_path = path_for_toml(&manifest.dir);
    let xtask_path = path_for_toml(&workspace_root.join("xtask"));
    let board_dependency = if manifest.host_feature {
        format!(
            "fstart_board = {{ package = \"{}\", path = \"{}\", default-features = false, features = [\"host\"] }}\n",
            manifest.package, board_path
        )
    } else {
        format!(
            "fstart_board = {{ package = \"{}\", path = \"{}\" }}\n",
            manifest.package, board_path
        )
    };
    let cargo_toml = format!(
        r#"[package]
name = "fstart-board-tool-{board}"
version = "0.0.0"
edition = "2021"
publish = false

[workspace]

[dependencies]
xtask = {{ path = "{xtask_path}" }}
{board_dependency}"#,
        board = manifest.board.replace('_', "-")
    );
    write_if_changed(&tool_dir.join("Cargo.toml"), &cargo_toml)?;

    let acpi_callback = if manifest.acpi_only_devices {
        "Some(fstart_board::acpi_only_devices)"
    } else {
        "None"
    };
    let main_rs = format!(
        r#"fn main() {{
    xtask::board_tool::main(xtask::board_tool::BoardCallbacks {{
        board_config: fstart_board::board_config,
        acpi_only_devices: {acpi_callback},
    }});
}}
"#
    );
    write_if_changed(&src_dir.join("main.rs"), &main_rs)?;

    let status = Command::new("cargo")
        .current_dir(&tool_dir)
        .arg("run")
        .arg("--quiet")
        .arg("--")
        .args(args)
        .env("FSTART_WORKSPACE_ROOT", workspace_root)
        .status()
        .map_err(|e| format!("failed to run board tool for {}: {e}", manifest.board))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "generated board tool for '{}' failed with {status}",
            manifest.board
        ))
    }
}

fn path_for_toml(path: &Path) -> String {
    path.display().to_string().replace('\\', "\\\\")
}

fn write_if_changed(path: &Path, content: &str) -> Result<(), String> {
    if fs::read_to_string(path).is_ok_and(|existing| existing == content) {
        return Ok(());
    }
    let mut file =
        fs::File::create(path).map_err(|e| format!("failed to create {}: {e}", path.display()))?;
    file.write_all(content.as_bytes())
        .map_err(|e| format!("failed to write {}: {e}", path.display()))
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

fn metadata_list(text: &str, key: &str) -> Vec<String> {
    let mut in_fstart_metadata = false;
    let mut collecting = false;
    let mut values = Vec::new();

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_fstart_metadata = trimmed == "[package.metadata.fstart]";
            collecting = false;
            continue;
        }
        if !in_fstart_metadata || trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }

        let list_text = if collecting {
            trimmed
        } else {
            let Some((found_key, value)) = trimmed.split_once('=') else {
                continue;
            };
            if found_key.trim() != key {
                continue;
            }
            collecting = true;
            value.trim()
        };

        for item in list_text
            .trim_matches(['[', ']'])
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
        {
            values.push(item.trim_matches('"').to_string());
        }

        if list_text.contains(']') {
            break;
        }
    }

    values
}

fn metadata_bool(text: &str, key: &str) -> Option<bool> {
    metadata_value(text, key).and_then(|value| value.parse().ok())
}

fn has_feature(text: &str, feature: &str) -> bool {
    let mut in_features = false;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_features = trimmed == "[features]";
            continue;
        }
        if !in_features || trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        let Some((found_key, _)) = trimmed.split_once('=') else {
            continue;
        };
        if found_key.trim() == feature {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::{metadata_list, metadata_value, read};
    use std::fs;

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

    #[test]
    fn reads_metadata_feature_list() {
        let text = r#"
            [package.metadata.fstart]
            features = [
              "intel-gm965",
              "intel-ich8",
            ]
        "#;

        assert_eq!(
            metadata_list(text, "features"),
            vec!["intel-gm965".to_string(), "intel-ich8".to_string()]
        );
    }

    #[test]
    fn reads_board_owned_stage_metadata() {
        let dir = std::env::temp_dir().join(format!(
            "fstart-board-stage-manifest-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        let manifest = dir.join("Cargo.toml");
        fs::write(
            &manifest,
            r#"
            [package]
            name = "fstart-board-lenovo-x61"

            [package.metadata.fstart]
            board = "lenovo-x61"
            target = "x86_64-unknown-none"
            stage-bin = "fstart-stage"
            "#,
        )
        .unwrap();

        let parsed = read(&manifest).unwrap();
        assert_eq!(parsed.board, "lenovo-x61");
        assert_eq!(parsed.package, "fstart-board-lenovo-x61");
        assert_eq!(parsed.stage_bin.as_deref(), Some("fstart-stage"));

        fs::remove_dir_all(dir).unwrap();
    }
}
