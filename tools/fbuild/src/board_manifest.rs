use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardManifest {
    /// Requested board id: the base board id, or the variant id when a
    /// variant was selected.
    pub board: String,
    pub package: String,
    pub dir: PathBuf,
    /// Board directory relative to `boards/`, e.g. `lenovo/x61`.
    pub rel_dir: PathBuf,
    pub platform: Option<String>,
    pub target: Option<String>,
    pub features: Vec<String>,
    /// Cargo features contributed by the selected variant (already merged
    /// into `features`); the host tool build needs them separately.
    pub variant_features: Vec<String>,
    /// Declared variants: variant id -> cargo features.
    pub variants: Vec<(String, Vec<String>)>,
    pub acpi_only_devices: bool,
    pub stage_bin: Option<String>,
}

/// Boards live at `boards/<vendor>/<board>/Cargo.toml`.
pub fn discover(workspace_root: &Path) -> Result<Vec<BoardManifest>, String> {
    let boards_dir = workspace_root.join("boards");
    let mut boards = Vec::new();

    for vendor_entry in fs::read_dir(&boards_dir)
        .map_err(|e| format!("failed to read {}: {e}", boards_dir.display()))?
    {
        let vendor_entry = vendor_entry.map_err(|e| format!("failed to read vendor dir: {e}"))?;
        let vendor_dir = vendor_entry.path();
        if !vendor_dir.is_dir() {
            continue;
        }
        for entry in fs::read_dir(&vendor_dir)
            .map_err(|e| format!("failed to read {}: {e}", vendor_dir.display()))?
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
    }

    boards.sort_by(|a, b| a.board.cmp(&b.board));
    Ok(boards)
}

/// Resolve a board or variant id to its manifest.
///
/// A variant id resolves to its board's manifest with `board` set to the
/// variant id and the variant's cargo features merged in.
pub fn find(workspace_root: &Path, board_name: &str) -> Result<BoardManifest, String> {
    let boards = discover(workspace_root)?;

    if let Some(board) = boards.iter().find(|b| b.board == board_name) {
        return Ok(board.clone());
    }

    for board in &boards {
        if let Some((variant, variant_features)) =
            board.variants.iter().find(|(name, _)| name == board_name)
        {
            let mut resolved = board.clone();
            resolved.board = variant.clone();
            resolved.variant_features = variant_features.clone();
            resolved.features.extend(variant_features.iter().cloned());
            return Ok(resolved);
        }
    }

    Err(format!(
        "board '{board_name}' not found: no boards/<vendor>/<board>/Cargo.toml declares it as \
         package.metadata.fstart.board or as a package.metadata.fstart.variants entry"
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

    let rel_dir = rel_dir_under_boards(&dir);

    Ok(BoardManifest {
        board,
        package,
        dir,
        rel_dir,
        platform: metadata_value(&text, "platform"),
        target: metadata_value(&text, "target"),
        features: metadata_list(&text, "features"),
        variant_features: Vec::new(),
        variants: metadata_variants(&text),
        acpi_only_devices: metadata_bool(&text, "acpi-only-devices").unwrap_or(false),
        stage_bin: metadata_value(&text, "stage-bin"),
    })
}

/// `<...>/boards/<vendor>/<board>` -> `<vendor>/<board>`; falls back to the
/// directory name when the manifest is not under a `boards/` root (tests).
fn rel_dir_under_boards(dir: &Path) -> PathBuf {
    let mut components: Vec<&std::ffi::OsStr> = Vec::new();
    for component in dir.iter().rev() {
        if component == "boards" {
            return components.iter().rev().collect();
        }
        components.push(component);
    }
    dir.file_name().map(PathBuf::from).unwrap_or_default()
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

/// Parse `[package.metadata.fstart.variants.<name>]` sections. Each section
/// currently supports a `features = [...]` list.
fn metadata_variants(text: &str) -> Vec<(String, Vec<String>)> {
    const PREFIX: &str = "[package.metadata.fstart.variants.";
    let mut variants: Vec<(String, Vec<String>)> = Vec::new();
    let mut current: Option<usize> = None;
    let mut collecting = false;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            collecting = false;
            current = None;
            if let Some(name) = trimmed
                .strip_prefix(PREFIX)
                .and_then(|rest| rest.strip_suffix(']'))
            {
                if !name.is_empty() && !name.contains('.') {
                    variants.push((name.to_string(), Vec::new()));
                    current = Some(variants.len() - 1);
                }
            }
            continue;
        }
        let Some(index) = current else {
            continue;
        };
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }

        let list_text = if collecting {
            trimmed
        } else {
            let Some((key, value)) = trimmed.split_once('=') else {
                continue;
            };
            if key.trim() != "features" {
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
            variants[index].1.push(item.trim_matches('"').to_string());
        }

        if list_text.contains(']') {
            collecting = false;
        }
    }

    variants
}

#[cfg(test)]
mod tests {
    use super::{find, metadata_list, metadata_value, metadata_variants, read};
    use std::fs;

    #[test]
    fn reads_variant_sections() {
        let text = r#"
            [package.metadata.fstart]
            board = "lenovo-x61"
            features = ["intel-gm965"]

            [package.metadata.fstart.variants.lenovo-x61s]
            features = ["variant-x61s"]

            [package.metadata.fstart.variants.lenovo-x61t]
            features = [
              "variant-x61t",
              "tablet",
            ]
        "#;

        assert_eq!(
            metadata_variants(text),
            vec![
                ("lenovo-x61s".to_string(), vec!["variant-x61s".to_string()]),
                (
                    "lenovo-x61t".to_string(),
                    vec!["variant-x61t".to_string(), "tablet".to_string()]
                ),
            ]
        );
        // The base metadata feature list is not polluted by variant lists.
        assert_eq!(metadata_list(text, "features"), vec!["intel-gm965"]);
    }

    #[test]
    fn finds_boards_and_variants_under_vendor_dirs() {
        let root =
            std::env::temp_dir().join(format!("fstart-board-vendor-test-{}", std::process::id()));
        let board_dir = root.join("boards").join("lenovo").join("x61");
        fs::create_dir_all(&board_dir).unwrap();
        fs::write(
            board_dir.join("Cargo.toml"),
            r#"
            [package]
            name = "fstart-board-lenovo-x61"

            [package.metadata.fstart]
            board = "lenovo-x61"
            features = ["intel-gm965"]

            [package.metadata.fstart.variants.lenovo-x61s]
            features = ["variant-x61s"]
            "#,
        )
        .unwrap();

        let base = find(&root, "lenovo-x61").unwrap();
        assert_eq!(base.board, "lenovo-x61");
        assert_eq!(base.rel_dir, std::path::PathBuf::from("lenovo/x61"));
        assert_eq!(base.features, vec!["intel-gm965"]);
        assert!(base.variant_features.is_empty());

        let variant = find(&root, "lenovo-x61s").unwrap();
        assert_eq!(variant.board, "lenovo-x61s");
        assert_eq!(variant.package, "fstart-board-lenovo-x61");
        assert_eq!(variant.variant_features, vec!["variant-x61s"]);
        assert_eq!(variant.features, vec!["intel-gm965", "variant-x61s"]);

        assert!(find(&root, "lenovo-x62").is_err());

        fs::remove_dir_all(root).unwrap();
    }

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
