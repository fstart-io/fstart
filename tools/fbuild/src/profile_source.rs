//! Resolve a selected profile through Cargo's direct-dependency graph once.

use crate::board_manifest::BoardManifest;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex, OnceLock},
};

pub(crate) struct ProfileSource {
    pub platform: serde_json::Value,
    pub package: serde_json::Value,
    pub metadata: Arc<serde_json::Value>,
}

/// Process-scoped cache of `cargo metadata --locked` documents, keyed by
/// canonical workspace root and the effective qualified feature arguments.
/// Base boards with empty variant features share one metadata invocation;
/// variants with distinct qualified features resolve independently.
#[derive(Default)]
struct MetadataCache {
    entries: Mutex<BTreeMap<(PathBuf, String), Arc<serde_json::Value>>>,
}

impl MetadataCache {
    fn get_or_load(
        &self,
        root: &Path,
        features: &str,
        fetch: impl FnOnce(&Path, &str) -> Result<serde_json::Value, String>,
    ) -> Result<Arc<serde_json::Value>, String> {
        let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let key = (canonical_root, features.to_owned());
        if let Some(cached) = self
            .entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .cloned()
        {
            return Ok(cached);
        }
        let metadata = Arc::new(fetch(root, features)?);
        let mut guard = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        Ok(guard.entry(key).or_insert(metadata).clone())
    }
}

fn shared_cache() -> &'static MetadataCache {
    static CACHE: OnceLock<MetadataCache> = OnceLock::new();
    CACHE.get_or_init(MetadataCache::default)
}

fn qualified_features(board: &BoardManifest) -> String {
    let mut features: Vec<String> = board
        .variant_features
        .iter()
        .map(|feature| format!("{}/{feature}", board.package))
        .collect();
    features.sort();
    features.dedup();
    features.join(",")
}

fn fetch_cargo_metadata(root: &Path, features: &str) -> Result<serde_json::Value, String> {
    let output = Command::new("cargo")
        .current_dir(root)
        .args([
            "metadata",
            "--locked",
            "--format-version=1",
            "--no-default-features",
            "--features",
            features,
        ])
        .arg("--manifest-path")
        .arg(root.join("Cargo.toml"))
        .output()
        .map_err(|e| format!("cargo metadata: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())
}

pub(crate) fn load_profile(root: &Path, board: &BoardManifest) -> Result<ProfileSource, String> {
    load_profile_with(shared_cache(), root, board, fetch_cargo_metadata)
}

fn load_profile_with(
    cache: &MetadataCache,
    root: &Path,
    board: &BoardManifest,
    fetch: impl FnOnce(&Path, &str) -> Result<serde_json::Value, String>,
) -> Result<ProfileSource, String> {
    let reference = board
        .build_profile
        .as_ref()
        .ok_or("missing build-profile")?;
    let features = qualified_features(board);
    let metadata = cache.get_or_load(root, &features, fetch)?;
    resolve_from_metadata(metadata, board, &reference.dependency)
}

fn resolve_from_metadata(
    metadata: Arc<serde_json::Value>,
    board: &BoardManifest,
    dependency: &str,
) -> Result<ProfileSource, String> {
    let packages = metadata["packages"]
        .as_array()
        .ok_or("Cargo metadata has no packages")?;
    let board_path = board
        .dir
        .join("Cargo.toml")
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let package = packages
        .iter()
        .find(|p| {
            p["manifest_path"]
                .as_str()
                .and_then(|s| Path::new(s).canonicalize().ok())
                .as_ref()
                == Some(&board_path)
        })
        .ok_or("selected board missing from Cargo metadata")?;
    let dependencies = package["dependencies"]
        .as_array()
        .ok_or("missing board dependencies")?;
    if !dependencies
        .iter()
        .any(|d| d["rename"].as_str().or(d["name"].as_str()) == Some(dependency))
    {
        return Err(format!(
            "build-profile dependency '{dependency}' is not direct"
        ));
    }
    let nodes = metadata["resolve"]["nodes"]
        .as_array()
        .ok_or("missing Cargo resolve graph")?;
    let node = nodes
        .iter()
        .find(|node| node["id"] == package["id"])
        .ok_or("missing board resolve node")?;
    let key = dependency.replace('-', "_");
    let dep = node["deps"]
        .as_array()
        .ok_or("missing resolved dependencies")?
        .iter()
        .find(|dep| dep["name"].as_str() == Some(&key))
        .ok_or("profile dependency is not enabled")?;
    let platform = packages
        .iter()
        .find(|p| p["id"] == dep["pkg"])
        .ok_or("missing platform package")?;
    Ok(ProfileSource {
        platform: platform.clone(),
        package: package.clone(),
        metadata,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board_manifest::ProfileRef;
    use std::cell::Cell;
    use std::fs;

    fn board(dir: &Path, id: &str, package: &str, dep: &str, features: &[&str]) -> BoardManifest {
        BoardManifest {
            board: id.into(),
            package: package.into(),
            dir: dir.to_path_buf(),
            rel_dir: PathBuf::from(id),
            arch: None,
            variant_features: features.iter().map(|s| (*s).into()).collect(),
            variants: BTreeMap::new(),
            build_profile: Some(ProfileRef {
                dependency: dep.into(),
            }),
            name: None,
            donor: None,
        }
    }

    #[test]
    fn metadata_cache_reuses_base_graph_and_separates_variant_features() {
        let tmp = tempfile::tempdir().unwrap();
        let a_dir = tmp.path().join("boards/v/a");
        let b_dir = tmp.path().join("boards/v/b");
        fs::create_dir_all(&a_dir).unwrap();
        fs::create_dir_all(&b_dir).unwrap();
        fs::write(a_dir.join("Cargo.toml"), "").unwrap();
        fs::write(b_dir.join("Cargo.toml"), "").unwrap();

        let a_manifest = a_dir.join("Cargo.toml").canonicalize().unwrap();
        let b_manifest = b_dir.join("Cargo.toml").canonicalize().unwrap();

        let metadata_for = |features: &str| {
            let platform_pkg = if features.contains("alt-platform") {
                "plat-alt"
            } else {
                "plat-base"
            };
            serde_json::json!({
                "packages": [
                    {
                        "id": "board-a",
                        "name": "board-a",
                        "manifest_path": a_manifest,
                        "dependencies": [{"name": "platform-crate", "rename": "renamed-plat"}]
                    },
                    {
                        "id": "board-b",
                        "name": "board-b",
                        "manifest_path": b_manifest,
                        "dependencies": [{"name": "platform-crate"}]
                    },
                    {"id": "plat-base", "name": "platform-crate"},
                    {"id": "plat-alt", "name": "platform-alt"}
                ],
                "resolve": {
                    "nodes": [
                        {
                            "id": "board-a",
                            "deps": [{"name": "renamed_plat", "pkg": platform_pkg}]
                        },
                        {
                            "id": "board-b",
                            "deps": [{"name": "platform_crate", "pkg": platform_pkg}]
                        }
                    ]
                }
            })
        };

        let cache = MetadataCache::default();
        let calls = Cell::new(0usize);
        let fetch = |_root: &Path, features: &str| {
            calls.set(calls.get() + 1);
            Ok(metadata_for(features))
        };

        // Two base boards (empty feature sets) share one metadata call, including a renamed dep.
        let board_a = board(&a_dir, "a", "board-a", "renamed-plat", &[]);
        let board_b = board(&b_dir, "b", "board-b", "platform-crate", &[]);
        let src_a = load_profile_with(&cache, tmp.path(), &board_a, fetch).unwrap();
        let src_b = load_profile_with(&cache, tmp.path(), &board_b, fetch).unwrap();
        assert_eq!(calls.get(), 1);
        assert_eq!(src_a.platform["id"], "plat-base");
        assert_eq!(src_b.platform["id"], "plat-base");
        assert!(Arc::ptr_eq(&src_a.metadata, &src_b.metadata));

        // A feature-changing variant gets its own cache entry and resolves its enabled dep.
        let variant_a = board(
            &a_dir,
            "a-alt",
            "board-a",
            "renamed-plat",
            &["alt-platform"],
        );
        let src_v = load_profile_with(&cache, tmp.path(), &variant_a, fetch).unwrap();
        assert_eq!(calls.get(), 2);
        assert_eq!(src_v.platform["id"], "plat-alt");
        assert!(!Arc::ptr_eq(&src_a.metadata, &src_v.metadata));
    }
}
