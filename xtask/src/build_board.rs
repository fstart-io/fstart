//! Board build orchestration.
//!
//! 1. Parse board.ron
//! 2. Determine target triple, cargo features, and environment
//! 3. Invoke cargo build on fstart-stage with the right env vars
//! 4. Return the path to the built binary

use fstart_codegen::ron_loader;
use fstart_types::StageLayout;
use std::path::PathBuf;
use std::process::Command;

/// Build firmware for the given board. Returns path to the output binary.
pub fn build(board_name: &str, release: bool) -> Result<PathBuf, String> {
    let workspace_root = workspace_root()?;
    let board_dir = workspace_root.join("boards").join(board_name);
    let board_ron = board_dir.join("board.ron");

    if !board_ron.exists() {
        return Err(format!("board config not found: {}", board_ron.display()));
    }

    eprintln!("[fstart] loading board config: {}", board_ron.display());
    let config = ron_loader::load_board_config(&board_ron)?;

    eprintln!("[fstart] board: {}", config.name);
    eprintln!("[fstart] platform: {}", config.platform);
    eprintln!("[fstart] mode: {:?}", config.mode);

    // Determine target triple
    let target = match config.platform.as_str() {
        "riscv64" => "riscv64gc-unknown-none-elf",
        "aarch64" => "aarch64-unknown-none",
        other => return Err(format!("unsupported platform: {other}")),
    };

    // Collect features: platform + drivers
    let mut features = Vec::new();
    features.push(config.platform.to_string());
    for device in &config.devices {
        features.push(device.driver.to_string());
    }
    let features_str = features.join(",");

    // Determine stage name for multi-stage builds
    let stage_name = match &config.stages {
        StageLayout::Monolithic(_) => None,
        StageLayout::MultiStage(stages) => {
            // For now, build the first stage. TODO: build all stages.
            stages.first().map(|s| s.name.to_string())
        }
    };

    eprintln!("[fstart] target: {target}");
    eprintln!("[fstart] features: {features_str}");

    let mut cmd = Command::new("cargo");
    cmd.arg("build")
        .arg("--package")
        .arg("fstart-stage")
        .arg("--target")
        .arg(target)
        .arg("--features")
        .arg(&features_str)
        .arg("-Z")
        .arg("build-std=core");

    if release {
        cmd.arg("--release");
    }

    // Disable UB precondition checks on volatile ops (nightly core library
    // adds alignment/null checks on read_volatile/write_volatile that are
    // incompatible with MMIO register access in firmware debug builds).
    cmd.env("RUSTFLAGS", "-Zub-checks=no");

    // Pass board RON path to build.rs
    cmd.env("FSTART_BOARD_RON", board_ron.to_str().unwrap());
    if let Some(ref name) = stage_name {
        cmd.env("FSTART_STAGE_NAME", name);
    }

    eprintln!("[fstart] building fstart-stage...");
    let status = cmd
        .status()
        .map_err(|e| format!("failed to run cargo: {e}"))?;
    if !status.success() {
        return Err("build failed".to_string());
    }

    // Determine output binary path
    let profile = if release { "release" } else { "debug" };
    let binary_path = workspace_root
        .join("target")
        .join(target)
        .join(profile)
        .join("fstart-stage");

    eprintln!("[fstart] built: {}", binary_path.display());
    Ok(binary_path)
}

fn workspace_root() -> Result<PathBuf, String> {
    // Walk up from current dir looking for the workspace Cargo.toml
    let mut dir = std::env::current_dir().map_err(|e| format!("no cwd: {e}"))?;
    loop {
        let cargo_toml = dir.join("Cargo.toml");
        if cargo_toml.exists() {
            let contents =
                std::fs::read_to_string(&cargo_toml).map_err(|e| format!("read error: {e}"))?;
            if contents.contains("[workspace]") {
                return Ok(dir);
            }
        }
        if !dir.pop() {
            return Err("could not find workspace root".to_string());
        }
    }
}
