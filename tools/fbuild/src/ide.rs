//! Selected Cargo editor view. This is not a workspace/lock ownership cutover.

use crate::{board_manifest::BoardManifest, selection::Selection};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Pin {
    name: String,
    version: String,
    source: Option<String>,
    checksum: Option<String>,
}

pub(crate) fn external_pins(text: &str) -> Result<BTreeSet<Pin>, String> {
    #[derive(Deserialize)]
    struct Lock {
        package: Vec<Pin>,
    }
    let lock: Lock = toml::from_str(text).map_err(|e| format!("Cargo.lock: {e}"))?;
    Ok(lock
        .package
        .into_iter()
        .filter(|p| p.source.is_some())
        .collect())
}

/// Generate an opt-in VS Code workspace and an editor-neutral RA configuration.
/// Does not modify editor settings in the source tree or remove any board locks.
pub fn generate_image(
    root: &Path,
    board: &BoardManifest,
    layout: &crate::resolved_image::ResolvedImage,
    name: Option<&str>,
    release: bool,
    audit_lock: bool,
) -> Result<PathBuf, String> {
    let layout = &layout.build;
    let unit = layout.selected(name)?;
    let selection = layout.selection(root, board, &unit.name, release)?;
    generate_selection(
        root,
        board,
        &unit.target,
        &layout.plan.payload,
        &unit.features,
        &unit.name,
        release,
        audit_lock,
        selection,
    )
}

fn generate_selection(
    root: &Path,
    board: &BoardManifest,
    target: &str,
    payload: &str,
    features: &[String],
    role: &str,
    release: bool,
    audit_lock: bool,
    selection: Selection,
) -> Result<PathBuf, String> {
    let mut command = Command::new("cargo");
    selection.apply_environment(&mut command);
    let output = command
        .current_dir(root)
        .args([
            "metadata",
            "--locked",
            "--format-version=1",
            "--no-default-features",
        ])
        .arg("--manifest-path")
        .arg(&selection.manifest)
        .arg("--filter-platform")
        .arg(target)
        .arg("--features")
        .arg(features.join(","))
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    let metadata: Value = serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?;
    let members: BTreeSet<&str> = metadata["workspace_members"]
        .as_array()
        .ok_or("missing Cargo members")?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let packages = metadata["packages"]
        .as_array()
        .ok_or("missing Cargo packages")?;
    let mut sources = Vec::new();
    for package in packages
        .iter()
        .filter(|p| members.contains(p["id"].as_str().unwrap_or("")))
    {
        let manifest = Path::new(
            package["manifest_path"]
                .as_str()
                .ok_or("missing manifest")?,
        )
        .canonicalize()
        .map_err(|e| e.to_string())?;
        if manifest.starts_with(root.join("boards"))
            && manifest
                != board
                    .dir
                    .join("Cargo.toml")
                    .canonicalize()
                    .map_err(|e| e.to_string())?
        {
            return Err(format!(
                "editor workspace contains unrelated board: {}",
                manifest.display()
            ));
        }
        sources.push(json!({"package": package["name"], "manifest": manifest}));
    }
    let selected_lock = fs::read_to_string(selection.manifest.with_file_name("Cargo.lock"))
        .map_err(|e| e.to_string())?;
    let root_lock = fs::read_to_string(root.join("Cargo.lock")).map_err(|e| e.to_string())?;
    let selected = external_pins(&selected_lock)?;
    let candidate = external_pins(&root_lock)?;
    let differences: Vec<_> = selected.difference(&candidate).collect();
    let project = crate::ide_graph::generate(root, &selection, &metadata)?;
    let lock_prototype = if audit_lock {
        Some(crate::ide_lock::audit(root, &selection)?)
    } else {
        None
    };
    let directory = root
        .join("target/fstart-ide")
        .join(&board.board)
        .join(if release { "release" } else { "debug" })
        .join(payload);
    let directory = if role == "stage" {
        directory
    } else {
        directory.join(role)
    };
    let settings = json!({
        "rust-analyzer.linkedProjects": [directory.join("rust-project.json")],
        "rust-analyzer.cargo.target": target,
        "rust-analyzer.cargo.cfgs": [],
        "rust-analyzer.cfg.setTest": false,
        "rust-analyzer.cargo.extraEnv": selection.environment(),
        "rust-analyzer.check.extraEnv": selection.environment(),
        "rust-analyzer.check.overrideCommand": selection.check_command(),
        "rust-analyzer.check.invocationStrategy": "once",
        "rust-analyzer.procMacro.enable": true
    });
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let workspace = directory.join("fstart.code-workspace");
    let write_json = |path: PathBuf, value: &Value| -> Result<(), String> {
        let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
        fs::write(
            &temporary,
            serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        fs::rename(temporary, path).map_err(|e| e.to_string())
    };
    write_json(directory.join("rust-project.json"), &project)?;
    write_json(
        workspace.clone(),
        &json!({"folders": [{"path": root}], "settings": settings}),
    )?;
    write_json(directory.join("rust-analyzer.json"), &settings)?;
    write_json(
        directory.join("report.json"),
        &json!({
            "board": board.board, "target": target, "payload": payload, "stage": role,
            "workspace_members": sources, "resolved_build": selection.directory.join("resolved-build.json"),
            "external_pins_match_root_candidate": differences.is_empty(),
            "external_pins_missing_from_root_candidate": differences,
            "all_board_lock_prototype": lock_prototype,
            "workspace_ownership_cutover": false
        }),
    )?;
    if !differences.is_empty() {
        eprintln!(
            "[fstart] editor lock differs from root candidate; see report.json (no lock cutover)"
        );
    }
    Ok(workspace)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lock_agreement_checks_version_source_and_checksum_not_local_workspace_membership() {
        let candidate = "[[package]]\nname='dep'\nversion='1.0.0'\nsource='registry+https://example.com'\nchecksum='abc'\n";
        let pins = external_pins(candidate).unwrap();
        let local = format!("{candidate}[[package]]\nname='board'\nversion='0.1.0'\n");
        assert_eq!(external_pins(&local).unwrap(), pins);
        for modified in [
            candidate.replace("1.0.0", "1.0.1"),
            candidate.replace("example.com", "other.com"),
            candidate.replace("abc", "def"),
        ] {
            assert_eq!(
                external_pins(&modified).unwrap().difference(&pins).count(),
                1
            );
        }
    }
}
