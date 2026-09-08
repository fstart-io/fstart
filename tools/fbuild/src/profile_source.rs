//! Resolve a selected profile through Cargo's direct-dependency graph once.

use crate::board_manifest::BoardManifest;
use std::{path::Path, process::Command};

pub(crate) struct ProfileSource {
    pub workspace: std::path::PathBuf,
    pub platform: serde_json::Value,
    pub package: serde_json::Value,
    pub metadata: serde_json::Value,
}

pub(crate) fn load_profile(root: &Path, board: &BoardManifest) -> Result<ProfileSource, String> {
    let reference = board
        .build_profile
        .as_ref()
        .ok_or("missing build-profile")?;
    let workspace = crate::build_board::prepare_selected_board_workspace(root, board)?;
    let output = Command::new("cargo")
        .current_dir(root)
        .args([
            "metadata",
            "--format-version=1",
            "--no-default-features",
            "--features",
            &std::iter::once(format!("{}/host", reference.dependency))
                .chain(board.variant_features.iter().cloned())
                .collect::<Vec<_>>()
                .join(","),
        ])
        .arg("--manifest-path")
        .arg(workspace.join("Cargo.toml"))
        .output()
        .map_err(|e| format!("cargo metadata: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?;
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
        .any(|d| d["rename"].as_str().or(d["name"].as_str()) == Some(&reference.dependency))
    {
        return Err(format!(
            "build-profile dependency '{}' is not direct",
            reference.dependency
        ));
    }
    let nodes = metadata["resolve"]["nodes"]
        .as_array()
        .ok_or("missing Cargo resolve graph")?;
    let node = nodes
        .iter()
        .find(|node| node["id"] == package["id"])
        .ok_or("missing board resolve node")?;
    let key = reference.dependency.replace('-', "_");
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
        workspace,
        platform: platform.clone(),
        package: package.clone(),
        metadata,
    })
}
