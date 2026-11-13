use crate::build_plan::ParsedBoard;
use crate::payload::{BuildArgs, PayloadChoice};
use fstart_core::StageLayout;

fn load(
    manifest: &crate::board_manifest::BoardManifest,
    payload: Option<PayloadChoice>,
    options: &BuildArgs,
) -> Result<(crate::board_manifest::BoardManifest, ParsedBoard), String> {
    let root = crate::build_board::workspace_root_pub()?;
    let resolved =
        crate::resolved_image::ResolvedImage::load(&root, manifest, options.selection(payload))?;
    let config = resolved.assembler_config(&manifest.board)?;
    Ok((
        manifest.clone(),
        ParsedBoard {
            config,
            acpi_only_devices: Vec::new(),
            resolved: Some(resolved),
        },
    ))
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
    let (manifest, parsed) = load(manifest, payload, options)?;
    let supplied = crate::assemble::supplied_inputs(kernel, firmware, fit, &options.inputs)?;
    Ok(assemble_loaded(
        &workspace_root,
        manifest,
        parsed,
        release,
        &supplied,
        options,
    )?
    .image)
}

fn assemble_loaded(
    workspace_root: &std::path::Path,
    manifest: crate::board_manifest::BoardManifest,
    parsed: ParsedBoard,
    release: bool,
    supplied: &std::collections::BTreeMap<String, std::path::PathBuf>,
    options: &BuildArgs,
) -> Result<crate::assemble::AssembledImage, String> {
    crate::assemble::assemble_with_parsed(
        workspace_root,
        manifest,
        parsed,
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
    let (manifest, parsed) = load(manifest, None, options)?;
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
            parsed.clone(),
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
    let res = crate::build_board::build_with_parsed(&workspace_root, &manifest, &parsed, release)?;
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
    let (manifest, parsed) = load(manifest, payload, options)?;
    let config = &parsed.config;
    let build_policy = config.build.clone();
    let platform = config.platform;
    let is_multi_stage = matches!(config.stages, StageLayout::MultiStage(_));
    let needs_x86_pflash = config.platform.is_x86();
    // Even a halt-only monolithic stage must receive its signed root and
    // directory when its fixed flow verifies boot media. Running the bare ELF
    // would leave its build-patched anchor and firmware window empty.
    let needs_firmware_image = config.build.qemu_machine.is_some()
        || matches!(&config.stages, StageLayout::Monolithic(stage)
            if stage.build.firmware_image.is_some() || stage.build.verify_firmware);
    let has_payload_blobs = kernel.is_some()
        || firmware.is_some()
        || fit.is_some()
        || config.payload.as_ref().is_some_and(|p| {
            p.firmware.is_some()
                || p.kernel_file.is_some()
                || p.fit_file.is_some()
                || p.kind == fstart_core::PayloadKind::FitImage
        });

    if is_multi_stage || has_payload_blobs || needs_x86_pflash || needs_firmware_image {
        let supplied = crate::assemble::supplied_inputs(kernel, firmware, fit, &options.inputs)?;
        let assembled = assemble_loaded(
            &workspace_root,
            manifest,
            parsed,
            release,
            &supplied,
            options,
        )?;
        // The QEMU runner composes its machine flash from the FFS
        // artefact, not from a composed final flash image.
        let run_image = assembled
            .ffs
            .clone()
            .unwrap_or_else(|| assembled.image.clone());
        // The x86 pflash combines the assembled FFS with the linked stage
        // ELF (reset vector, anchor). The plan layout hashes unit dirs, so
        // the ELF travels with the build result instead of a guessed path.
        let x86_stage_elf = needs_x86_pflash
            .then(|| assembled.stages.first().map(|stage| stage.path.as_path()))
            .flatten();
        crate::qemu::run(
            &build_policy,
            platform,
            &run_image,
            disk,
            memory,
            secure_firmware,
            x86_stage_elf,
        )
    } else {
        let res =
            crate::build_board::build_with_parsed(&workspace_root, &manifest, &parsed, release)?;
        crate::qemu::run(
            &build_policy,
            platform,
            &res.primary_binary().run_path,
            disk,
            memory,
            secure_firmware,
            None,
        )
    }
}
