//! Exact firmware editor view projected from the workspace's selected Cargo unit.

use crate::board_manifest::BoardManifest;
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

/// Generate editor settings for the same compiler selection fbuild checks.
/// Source manifests and Cargo.lock remain owned by the root workspace.
pub fn generate_image(
    root: &Path,
    board: &BoardManifest,
    layout: &crate::resolved_image::ResolvedImage,
    name: Option<&str>,
    release: bool,
) -> Result<PathBuf, String> {
    let layout = &layout.build;
    let unit = layout.selected(name)?;
    let selection = layout.selection(root, board, &unit.name, release)?;
    let target = unit.target.as_str();
    let payload = layout.plan.payload.as_str();
    let role = unit.name.as_str();
    let features = &unit.features;
    let mut command = Command::new("cargo");
    selection.apply_environment(&mut command);
    let features = features
        .iter()
        .map(|feature| {
            if feature.contains('/') {
                feature.clone()
            } else {
                format!("{}/{feature}", board.package)
            }
        })
        .collect::<Vec<_>>()
        .join(",");
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
        .args(["--features", &features])
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    let metadata: Value = serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?;
    let project = crate::ide_graph::generate(root, &selection, &metadata)?;
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
        "rust-analyzer.check.workspace": false,
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
            "manifest": selection.manifest, "lock": root.join("Cargo.lock"),
            "resolved_build": selection.directory.join("resolved-build.json"),
        }),
    )?;
    Ok(workspace)
}
