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

/// Run board policy and protocol tests with the selected variant's features.
/// Firmware build selections remain separate: a host test must not inherit a
/// stage's linker script, target or generated artifact bindings.
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
        .args([
            "test",
            "--locked",
            "--lib",
            "--no-default-features",
            "--manifest-path",
        ])
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
    let resolved = crate::resolved_image::ResolvedImage::load(
        &workspace_root,
        manifest,
        options.selection(payload),
    )?;
    let amd = match &resolved.build.plan.assembly {
        fstart_image_build::build_plan::Assembly::Amd(image) => Some(image.clone()),
        _ => None,
    };
    // A PSP partition without an FFS: just the stages.
    if let Some(image) = amd.as_ref().filter(|image| image.ffs.is_none()) {
        if kernel.is_some() || firmware.is_some() || fit.is_some() {
            return Err("PSP image operation does not consume payload-file overrides".into());
        }
        let build = resolved.build(&workspace_root, manifest, release)?;
        let directory = resolved
            .artifact_dir(&workspace_root, &manifest.board, release)?
            .join("image");
        std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
        return crate::amd_image::compose(
            &workspace_root,
            &manifest.board,
            &directory,
            image,
            &build.stages,
            None,
        );
    }
    let config = resolved.assembler_config(&manifest.board)?;
    let sp3_flash = match &resolved.build.plan.assembly {
        fstart_image_build::build_plan::Assembly::Sp3Flash(image) => Some(image.clone()),
        _ => None,
    };
    let parsed = ParsedBoard {
        config,
        acpi_only_devices: Vec::new(),
        resolved: Some(resolved),
    };
    let assembled = assemble_loaded(
        &workspace_root,
        manifest.clone(),
        parsed,
        release,
        kernel,
        firmware,
        fit,
        options,
    )?;
    if let Some(image) = sp3_flash {
        return compose_sp3_flash(&workspace_root, &image, &assembled);
    }
    match amd {
        Some(image) => crate::amd_image::compose(
            &workspace_root,
            &manifest.board,
            assembled
                .image
                .parent()
                .ok_or("FFS image has no directory")?,
            &image,
            &assembled.stages,
            Some(&assembled.image),
        ),
        None => Ok(assembled.image),
    }
}

/// Pack the SP3 halves from their files and compose them with the
/// assembled FFS and the stage unit (`fstart_image_build::sp3_flash`).
/// Writes and returns `<board>.rom`, 16 MiB per packed half.
fn compose_sp3_flash(
    workspace_root: &std::path::Path,
    image: &fstart_image_build::sp3_flash::Sp3FlashAssembly,
    assembled: &crate::assemble::AssembledImage,
) -> Result<std::path::PathBuf, String> {
    use fstart_image_build::sp3_flash::{HALF_SIZE, HalfAction, name};
    let read = |path: &std::path::Path| {
        std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
    };
    let plan = &image.compose;
    for half in &image.packed {
        eprintln!(
            "[fstart] SP3 {} PSP firmware packed from its files",
            name(half.generation)
        );
    }
    let ffs = read(&assembled.image)?;
    let reset = assembled
        .stages
        .iter()
        .find(|stage| stage.name == image.stage_unit)
        .ok_or("SP3 stage unit was not built")?;
    let flash = image.assemble(
        &|path| read(&workspace_root.join(path)),
        &read(&reset.run_path)?,
        &ffs,
    )?;
    if !flash.patched.locator || !flash.patched.flash {
        eprintln!(
            "[fstart] warning: the SP3 stage unit links no FFS locator or flash anchor; it cannot read its FFS"
        );
    }
    let survey = &flash.survey;
    for (half, action) in &survey.halves {
        match action {
            HalfAction::Compose(layout) => eprintln!(
                "[fstart] SP3 {half:?} half ({}): EFS and directories to {:#x}",
                name(layout.generation),
                layout.prefix_end,
            ),
            HalfAction::Keep(generation) => eprintln!(
                "[fstart] warning: SP3 {half:?} half kept ({}: not supported by the board)",
                generation.map_or("no EFS", name),
            ),
        }
    }
    let reset_copies = if survey.composed().count() == 1 || flash.image.len() == HALF_SIZE {
        "one composed half"
    } else {
        "each half"
    };
    let out = assembled.image.with_extension("rom");
    std::fs::write(&out, &flash.image).map_err(|e| format!("{}: {e}", out.display()))?;
    eprintln!(
        "[fstart] SP3 image: {} ({} MiB); FFS {:#x} of {:#x} bytes at {:#x}..{:#x}; reset image {:#x} bytes at {:#x} ({reset_copies})",
        out.display(),
        flash.image.len() >> 20,
        ffs.len(),
        survey.ffs_capacity(),
        survey.ffs.start,
        survey.ffs.end,
        plan.reset.size,
        plan.reset.flash_offset,
    );
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn assemble_loaded(
    workspace_root: &std::path::Path,
    manifest: crate::board_manifest::BoardManifest,
    parsed: ParsedBoard,
    release: bool,
    kernel: Option<&str>,
    firmware: Option<&str>,
    fit: Option<&str>,
    options: &BuildArgs,
) -> Result<crate::assemble::AssembledImage, String> {
    crate::assemble::assemble_with_parsed(
        workspace_root,
        manifest,
        parsed,
        release,
        kernel,
        firmware,
        fit,
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
        let ffs_path = assemble_loaded(
            &workspace_root,
            manifest.clone(),
            parsed.clone(),
            release,
            None,
            None,
            None,
            options,
        )?
        .image;

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
        let assembled = assemble_loaded(
            &workspace_root,
            manifest,
            parsed,
            release,
            kernel,
            firmware,
            fit,
            options,
        )?;
        // The x86 pflash combines the assembled FFS with the linked stage
        // ELF (reset vector, anchor). The plan layout hashes unit dirs, so
        // the ELF travels with the build result instead of a guessed path.
        let x86_stage_elf = needs_x86_pflash
            .then(|| assembled.stages.first().map(|stage| stage.path.as_path()))
            .flatten();
        crate::qemu::run(
            &build_policy,
            platform,
            &assembled.image,
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
