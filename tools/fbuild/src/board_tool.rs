use crate::payload::{BuildArgs, PayloadChoice};
use crate::resolved_image::ResolvedImage;

fn load(
    manifest: &crate::board_manifest::BoardManifest,
    payload: Option<PayloadChoice>,
    options: &BuildArgs,
) -> Result<ResolvedImage, String> {
    let root = crate::build_board::workspace_root_pub()?;
    ResolvedImage::load(&root, manifest, options.selection(payload))
}

/// Run board policy and protocol tests with the board's default and the
/// selected variant's features. Firmware build selections remain separate: a
/// host test must not inherit a stage's linker script, target or generated
/// artifact bindings.
pub fn test_library(manifest: &crate::board_manifest::BoardManifest) -> Result<(), String> {
    let root = crate::build_board::workspace_root_pub()?;
    let version = std::process::Command::new("rustc")
        .arg("-vV")
        .output()
        .map_err(|e| format!("query host compiler: {e}"))?;
    if !version.status.success() {
        return Err("cannot query the Rust host target".into());
    }
    let version = String::from_utf8(version.stdout).map_err(|e| e.to_string())?;
    let host = version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .ok_or("rustc did not report its host target")?;
    let mut command = std::process::Command::new("cargo");
    command
        .current_dir(&root)
        .args(["test", "--locked", "--lib", "--manifest-path"])
        .arg(root.join("Cargo.toml"))
        .args(["--package", &manifest.package, "--target", host])
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env("RUSTFLAGS", "");
    if !manifest.variant_features.is_empty() {
        command.args(["--features", &manifest.variant_features.join(",")]);
    }
    let status = command
        .status()
        .map_err(|e| format!("board library tests: {e}"))?;
    if !status.success() {
        return Err(format!("{} library tests failed: {status}", manifest.board));
    }
    Ok(())
}

pub fn build(
    manifest: &crate::board_manifest::BoardManifest,
    release: bool,
    payload: Option<PayloadChoice>,
    options: &BuildArgs,
) -> Result<crate::build_board::BuildResult, String> {
    let root = crate::build_board::workspace_root_pub()?;
    let resolved =
        crate::resolved_image::ResolvedImage::load(&root, manifest, options.selection(payload))?;
    resolved.build(&root, manifest, release)
}

#[allow(clippy::too_many_arguments)] // the CLI's flat assemble options
pub fn assemble(
    manifest: &crate::board_manifest::BoardManifest,
    release: bool,
    payload: Option<PayloadChoice>,
    kernel: Option<&str>,
    firmware: Option<&str>,
    fit: Option<&str>,
    options: &BuildArgs,
) -> Result<std::path::PathBuf, String> {
    let workspace_root = crate::build_board::workspace_root_pub()?;
    let resolved = load(manifest, payload, options)?;
    let supplied = crate::assemble::supplied_inputs(kernel, firmware, fit, &options.inputs)?;
    Ok(assemble_loaded(
        &workspace_root,
        manifest.clone(),
        &resolved,
        release,
        &supplied,
        options,
    )?
    .image)
}

fn assemble_loaded(
    workspace_root: &std::path::Path,
    manifest: crate::board_manifest::BoardManifest,
    resolved: &ResolvedImage,
    release: bool,
    supplied: &std::collections::BTreeMap<String, std::path::PathBuf>,
    options: &BuildArgs,
) -> Result<crate::assemble::AssembledImage, String> {
    crate::assemble::assemble_with_resolved(
        workspace_root,
        &manifest,
        resolved,
        release,
        supplied,
        &options.payload_files,
        options.signing_key.as_deref(),
    )
}

pub fn flash(
    manifest: &crate::board_manifest::BoardManifest,
    release: bool,
    options: &BuildArgs,
    probe_run: bool,
    chip: Option<&str>,
    probe_selector: Option<&str>,
    base_address: Option<&str>,
) -> Result<(), String> {
    let workspace_root = crate::build_board::workspace_root_pub()?;
    let resolved = load(manifest, None, options)?;
    let manifest = manifest.clone();
    let chip_name = chip.unwrap_or("auto");
    let probe_rs = find_probe_rs().map_err(|e| format!("probe-rs not found: {e}"))?;

    eprintln!("[fstart] using probe-rs: {}", probe_rs.display());
    eprintln!("[fstart] chip: {chip_name}");

    let mk_cmd = |subcmd: &str| -> std::process::Command {
        let mut cmd = std::process::Command::new(&probe_rs);
        cmd.arg(subcmd)
            .arg("--chip")
            .arg(chip_name)
            .arg("--protocol")
            .arg("jtag");
        if let Some(sel) = probe_selector {
            cmd.arg("--probe").arg(sel);
        }
        cmd
    };

    if !probe_run {
        let base_address = base_address.ok_or_else(|| {
            "flash requires --base-address because fbuild has no board flash programmer defaults"
                .to_string()
        })?;
        eprintln!("[fstart] step 1/2: assembling FFS and flashing target memory...");
        let supplied = crate::assemble::supplied_inputs(None, None, None, &options.inputs)?;
        let assembled = assemble_loaded(
            &workspace_root,
            manifest.clone(),
            &resolved,
            release,
            &supplied,
            options,
        )?;
        let ffs_path = assembled.ffs.unwrap_or(assembled.image);

        let ffs_size = std::fs::metadata(&ffs_path).map(|m| m.len()).unwrap_or(0);
        eprintln!(
            "[fstart] flashing FFS ({:.1} MiB) to {base_address}...",
            ffs_size as f64 / (1024.0 * 1024.0)
        );

        let mut cmd = mk_cmd("download");
        cmd.arg("--binary-format")
            .arg("bin")
            .arg("--base-address")
            .arg(base_address)
            .arg(&ffs_path);

        eprintln!("[fstart] running: {:?}", cmd);
        let status = cmd
            .status()
            .map_err(|e| format!("failed to run probe-rs download: {e}"))?;
        if !status.success() {
            return Err(format!("probe-rs download failed with {status}"));
        }
        eprintln!("[fstart] target flash complete.");
    } else {
        eprintln!("[fstart] --probe-run: skipping image download");
    }

    eprintln!("[fstart] step 2/2: loading stage via probe-rs...");
    let res = resolved.build(&workspace_root, &manifest, release)?;
    let elf_path = &res.primary_binary().path;
    eprintln!("[fstart] ELF: {}", elf_path.display());

    let mut cmd = mk_cmd("run");
    cmd.arg(elf_path);

    eprintln!("[fstart] running: {:?}", cmd);
    let status = cmd
        .status()
        .map_err(|e| format!("failed to run probe-rs run: {e}"))?;
    if !status.success() {
        return Err(format!("probe-rs run failed with {status}"));
    }
    Ok(())
}

fn find_probe_rs() -> Result<std::path::PathBuf, String> {
    let home = std::env::var("HOME").unwrap_or_default();
    let local_paths = [
        format!("{home}/src/probe-rs/target/release/probe-rs"),
        format!("{home}/src/probe-rs/target/debug/probe-rs"),
    ];
    for p in &local_paths {
        let path = std::path::PathBuf::from(p);
        if path.exists() {
            return Ok(path);
        }
    }

    which_in_path("probe-rs").ok_or_else(|| "not in PATH or ~/src/probe-rs/target".to_string())
}

fn which_in_path(name: &str) -> Option<std::path::PathBuf> {
    let path_var = std::env::var("PATH").ok()?;
    for dir in path_var.split(':') {
        let candidate = std::path::PathBuf::from(dir).join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[allow(clippy::too_many_arguments)] // the CLI's flat run options
pub fn run(
    manifest: &crate::board_manifest::BoardManifest,
    release: bool,
    payload: Option<PayloadChoice>,
    kernel: Option<&str>,
    firmware: Option<&str>,
    fit: Option<&str>,
    disk: Option<&str>,
    memory: Option<&str>,
    secure_firmware: Option<&str>,
    options: &BuildArgs,
) -> Result<(), String> {
    let workspace_root = crate::build_board::workspace_root_pub()?;
    let resolved = load(manifest, payload, options)?;
    let manifest = manifest.clone();
    // The plan states how (and whether) this board runs under QEMU; there
    // is no per-architecture fallback launch to guess.
    let recipe = resolved.build.plan.run.clone().ok_or_else(|| {
        format!(
            "board '{}' has no QEMU run recipe in its plan",
            manifest.board
        )
    })?;
    let external = secure_firmware
        .map(|path| {
            std::collections::BTreeMap::from([(
                "secure-firmware".to_string(),
                std::path::PathBuf::from(path),
            )])
        })
        .unwrap_or_default();
    if resolved.build.plan.images.is_empty() {
        let res = resolved.build(&workspace_root, &manifest, release)?;
        let primary = res.primary_binary();
        return crate::qemu::run(
            &recipe,
            &crate::qemu::RunArtifacts {
                ffs: None,
                image: None,
                stage_flat: &primary.run_path,
                stages: &res.stages,
            },
            &external,
            disk,
            memory,
        );
    }
    let supplied = crate::assemble::supplied_inputs(kernel, firmware, fit, &options.inputs)?;
    let assembled = assemble_loaded(
        &workspace_root,
        manifest,
        &resolved,
        release,
        &supplied,
        options,
    )?;
    crate::qemu::run(
        &recipe,
        &crate::qemu::RunArtifacts {
            ffs: assembled.ffs.as_deref(),
            image: Some(&assembled.image),
            stage_flat: &assembled.stages[0].run_path,
            stages: &assembled.stages,
        },
        &external,
        disk,
        memory,
    )
}
