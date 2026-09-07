use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

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
    pub build_profile: Option<crate::resolved::ProfileRef>,
    pub layout: crate::resolved::Overrides,
}

// Only fstart's tables are closed. Cargo and other tools own the rest of the
// manifest, so their keys must remain unrestricted here.
#[derive(Deserialize)]
struct CargoManifest {
    package: Package,
}

#[derive(Deserialize)]
struct Package {
    name: String,
    metadata: PackageMetadata,
}

#[derive(Deserialize)]
struct PackageMetadata {
    fstart: BoardMetadata,
}

/// Discovery and opt-in resolved build metadata. Legacy boards keep their
/// existing host path until their scope is migrated.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct BoardMetadata {
    schema: u32,
    board: String,
    platform: Option<String>,
    target: Option<String>,
    #[serde(default)]
    features: Vec<String>,
    #[serde(default)]
    variants: BTreeMap<String, VariantMetadata>,
    #[serde(default)]
    acpi_only_devices: bool,
    stage_bin: Option<String>,
    build_profile: Option<crate::resolved::ProfileRef>,
    #[serde(default)]
    layout: crate::resolved::Overrides,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VariantMetadata {
    #[serde(default)]
    features: Vec<String>,
}

/// Boards live at `boards/<vendor>/<board>/Cargo.toml`.
///
/// Validate the whole identity namespace before resolving any selection, so a
/// duplicate can never be hidden by directory order or base-board precedence.
pub fn discover(workspace_root: &Path) -> Result<Vec<BoardManifest>, String> {
    let boards_dir = workspace_root.join("boards");
    let mut manifests = Vec::new();

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
                manifests.push(manifest);
            }
        }
    }

    // Stable diagnostics even if several manifests are invalid.
    manifests.sort();
    let mut boards = manifests
        .iter()
        .map(|manifest| read(manifest))
        .collect::<Result<Vec<_>, _>>()?;
    validate_identities(&boards)?;
    boards.sort_by(|a, b| a.board.cmp(&b.board));
    Ok(boards)
}

fn validate_identities(boards: &[BoardManifest]) -> Result<(), String> {
    let mut owners = BTreeMap::new();
    for board in boards {
        for id in std::iter::once(&board.board).chain(board.variants.iter().map(|(id, _)| id)) {
            if let Some(previous) = owners.insert(id, &board.dir) {
                return Err(format!(
                    "duplicate board/variant id '{id}' in {} and {}",
                    previous.join("Cargo.toml").display(),
                    board.dir.join("Cargo.toml").display(),
                ));
            }
        }
    }
    Ok(())
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
    parse(&text, manifest)
}

fn parse(text: &str, manifest: &Path) -> Result<BoardManifest, String> {
    let parsed: CargoManifest = toml::from_str(text)
        .map_err(|e| format!("invalid board manifest {}: {e}", manifest.display()))?;
    let metadata = parsed.package.metadata.fstart;
    if metadata.schema != 1 {
        return Err(format!(
            "unsupported fstart schema {} in {} (expected 1)",
            metadata.schema,
            manifest.display(),
        ));
    }
    if metadata.build_profile.is_none() && metadata.layout != crate::resolved::Overrides::default()
    {
        return Err(format!(
            "layout overrides require build-profile in {}",
            manifest.display()
        ));
    }
    for id in std::iter::once(&metadata.board).chain(metadata.variants.keys()) {
        // Identity becomes an artifact/workspace path component. Do not accept
        // path traversal, separators, whitespace or non-portable spellings.
        if id.is_empty()
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(format!(
                "invalid board/variant id '{id}' in {}: expected ASCII letters, digits, '-' or '_'",
                manifest.display(),
            ));
        }
    }
    let dir = manifest
        .parent()
        .ok_or_else(|| format!("manifest has no parent: {}", manifest.display()))?
        .to_path_buf();
    let rel_dir = rel_dir_under_boards(&dir);

    Ok(BoardManifest {
        board: metadata.board,
        package: parsed.package.name,
        dir,
        rel_dir,
        platform: metadata.platform,
        target: metadata.target,
        features: metadata.features,
        variant_features: Vec::new(),
        variants: metadata
            .variants
            .into_iter()
            .map(|(id, v)| (id, v.features))
            .collect(),
        acpi_only_devices: metadata.acpi_only_devices,
        stage_bin: metadata.stage_bin,
        build_profile: metadata.build_profile,
        layout: metadata.layout,
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

#[cfg(test)]
mod tests {
    use super::{discover, find, parse};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    const MANIFEST: &str = r#"
        [package] # Cargo owns keys outside fstart metadata
        name = 'fstart-board-test'
        edition = '2024'
        [package.metadata.other-tool]
        anything = true
        [package.metadata.fstart] # inline comments are ordinary TOML
        schema = 1
        board = 'test-board'
        platform = 'riscv64'
        target = 'riscv64gc-unknown-none-elf'
        stage-bin = 'fstart-stage'
        acpi-only-devices = true
        features = [
            'base', # commas and closing brackets in comments must not leak: , ]
            "second",
        ]
        [package.metadata.fstart.variants."test-variant"]
        features = [
            'variant-test', # comment
        ]
    "#;

    struct Inventory(PathBuf);

    impl Inventory {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            Self(std::env::temp_dir().join(format!(
                "fstart-discovery-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            )))
        }

        fn add(&self, dir: &str, text: &str) {
            let dir = self.0.join("boards").join(dir);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("Cargo.toml"), text).unwrap();
        }
    }

    impl Drop for Inventory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn parses_toml_without_leaking_cargo_or_variant_fields() {
        let board = parse(MANIFEST, Path::new("boards/vendor/test/Cargo.toml")).unwrap();
        assert_eq!(board.board, "test-board");
        assert_eq!(board.package, "fstart-board-test");
        assert_eq!(board.rel_dir, Path::new("vendor/test"));
        assert_eq!(board.platform.as_deref(), Some("riscv64"));
        assert_eq!(board.target.as_deref(), Some("riscv64gc-unknown-none-elf"));
        assert_eq!(board.stage_bin.as_deref(), Some("fstart-stage"));
        assert!(board.acpi_only_devices);
        assert_eq!(board.features, ["base", "second"]);
        assert_eq!(
            board.variants,
            [("test-variant".into(), vec!["variant-test".into()])]
        );
    }

    #[test]
    fn rejects_invalid_metadata_with_manifest_context() {
        for (text, diagnostic) in [
            (
                MANIFEST.replace("schema = 1", "schema = 2"),
                "unsupported fstart schema 2",
            ),
            (MANIFEST.replace("schema = 1", ""), "missing field `schema`"),
            (
                MANIFEST.replace("board = 'test-board'", ""),
                "missing field `board`",
            ),
            (
                MANIFEST.replace("acpi-only-devices = true", "acpi-only-devices = 'true'"),
                "invalid type",
            ),
            (
                MANIFEST.replace("acpi-only-devices = true", "acpi-only-device = true"),
                "unknown field",
            ),
            (
                MANIFEST.replace("features = [", "featuers = ["),
                "unknown field",
            ),
            (MANIFEST.replace("'variant-test',", "42,"), "invalid type"),
            (
                MANIFEST.replace("'variant-test',", "'variant-test'\n unexpected = true,"),
                "invalid",
            ),
            (
                MANIFEST.replace("board = 'test-board'", "board = '../escape'"),
                "invalid board/variant id",
            ),
            (
                MANIFEST.replace("test-variant", "../escape"),
                "invalid board/variant id",
            ),
            (
                MANIFEST.replace("board = 'test-board'", "board = ''"),
                "invalid board/variant id",
            ),
        ] {
            let error = parse(&text, Path::new("broken/Cargo.toml")).unwrap_err();
            assert!(error.contains("broken/Cargo.toml"), "{error}");
            assert!(error.contains(diagnostic), "expected {diagnostic}: {error}");
        }
        let error = parse(
            &format!("{MANIFEST}\nmisspelled = true"),
            Path::new("Cargo.toml"),
        )
        .unwrap_err();
        assert!(error.contains("unknown field `misspelled`"), "{error}");
    }

    #[test]
    fn discovers_excluded_boards_and_resolves_variants() {
        let inventory = Inventory::new();
        // Deliberately no workspace manifest or board compilation.
        inventory.add("vendor/test", MANIFEST);
        let base = find(&inventory.0, "test-board").unwrap();
        assert_eq!(base.features, ["base", "second"]);
        assert!(base.variant_features.is_empty());
        let variant = find(&inventory.0, "test-variant").unwrap();
        assert_eq!(variant.board, "test-variant");
        assert_eq!(variant.package, base.package);
        assert_eq!(variant.rel_dir, Path::new("vendor/test"));
        assert_eq!(variant.features, ["base", "second", "variant-test"]);
        assert_eq!(variant.variant_features, ["variant-test"]);
        assert!(find(&inventory.0, "missing").is_err());
    }

    #[test]
    fn rejects_all_identity_collision_shapes() {
        for second in [
            // base/base
            MANIFEST.replace("test-variant", "other-variant"),
            // variant/variant
            MANIFEST.replace("test-board", "other-board"),
            // base/variant
            MANIFEST
                .replace("test-board", "test-variant")
                .replace("variants.\"test-variant\"", "variants.\"other-variant\""),
        ] {
            let inventory = Inventory::new();
            inventory.add("a/one", MANIFEST);
            inventory.add("z/two", &second);
            let error = find(&inventory.0, "test-board").unwrap_err();
            assert!(error.contains("duplicate board/variant id"), "{error}");
            assert!(error.contains("a/one/Cargo.toml"), "{error}");
            assert!(error.contains("z/two/Cargo.toml"), "{error}");
        }
        let inventory = Inventory::new();
        inventory.add(
            "vendor/test",
            &MANIFEST.replace("test-variant", "test-board"),
        );
        assert!(
            discover(&inventory.0)
                .unwrap_err()
                .contains("duplicate board/variant id")
        );
    }

    #[test]
    fn discovers_current_inventory_in_identity_order() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let boards = discover(&root).unwrap();
        assert!(!boards.is_empty());
        assert!(boards.windows(2).all(|pair| pair[0].board < pair[1].board));
        for board in &boards {
            assert_eq!(find(&root, &board.board).unwrap().package, board.package);
            for (variant, _) in &board.variants {
                assert_eq!(find(&root, variant).unwrap().package, board.package);
            }
        }
    }
}
