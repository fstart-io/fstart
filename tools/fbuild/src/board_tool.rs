use clap::{Parser, Subcommand};

use crate::build_plan::ParsedBoard;
use crate::payload::{BuildArgs, PayloadChoice};
use fstart_core::StageLayout;

#[derive(Parser)]
#[command(name = "fbuild", about = "fstart firmware build tool")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Build {
        #[arg(short, long, default_value_t = false)]
        release: bool,
        #[arg(long, value_enum)]
        payload: Option<PayloadChoice>,
        #[command(flatten)]
        options: BuildArgs,
    },
    Run {
        #[arg(short, long, default_value_t = false)]
        release: bool,
        #[arg(long, value_enum)]
        payload: Option<PayloadChoice>,
        #[command(flatten)]
        options: BuildArgs,
        #[arg(short, long)]
        kernel: Option<String>,
        #[arg(short, long)]
        firmware: Option<String>,
        #[arg(long)]
        fit: Option<String>,
        #[arg(short, long)]
        disk: Option<String>,
        #[arg(short, long)]
        memory: Option<String>,
        /// Complete secure pflash image used only by QEMU SBSA-ref.
        #[arg(long)]
        secure_firmware: Option<String>,
    },
    Test {
        #[command(flatten)]
        options: BuildArgs,
    },
    Assemble {
        #[arg(short, long, default_value_t = false)]
        release: bool,
        #[arg(long, value_enum)]
        payload: Option<PayloadChoice>,
        #[command(flatten)]
        options: BuildArgs,
        #[arg(short, long)]
        kernel: Option<String>,
        #[arg(short, long)]
        firmware: Option<String>,
        #[arg(long)]
        fit: Option<String>,
    },
    Flash {
        #[arg(short, long, default_value_t = false)]
        release: bool,
        #[command(flatten)]
        options: BuildArgs,
        #[arg(long, default_value_t = false)]
        probe_run: bool,
        #[arg(long)]
        chip: Option<String>,
        #[arg(long)]
        probe: Option<String>,
        #[arg(long)]
        base_address: Option<String>,
    },
}

/// Run a migrated board directly in fbuild; no board Rust is built on the host.
pub fn run_metadata(
    manifest: &crate::board_manifest::BoardManifest,
    args: &[String],
) -> Result<(), String> {
    let cli = Cli::try_parse_from(std::iter::once("fbuild").chain(args.iter().map(String::as_str)))
        .map_err(|e| e.to_string())?;
    dispatch(cli, manifest)
}

fn dispatch(cli: Cli, manifest: &crate::board_manifest::BoardManifest) -> Result<(), String> {
    match cli.command {
        Command::Build {
            release,
            payload,
            options,
        } => build(manifest, release, payload, &options).map(|_| ()),
        Command::Run {
            release,
            payload,
            options,
            kernel,
            firmware,
            fit,
            disk,
            memory,
            secure_firmware,
        } => run(
            manifest,
            release,
            payload,
            kernel.as_deref(),
            firmware.as_deref(),
            fit.as_deref(),
            disk.as_deref(),
            memory.as_deref(),
            secure_firmware.as_deref(),
            &options,
        ),
        Command::Test { options } => run(
            manifest, true, None, None, None, None, None, None, None, &options,
        ),
        Command::Assemble {
            release,
            payload,
            options,
            kernel,
            firmware,
            fit,
        } => assemble(
            manifest,
            release,
            payload,
            kernel.as_deref(),
            firmware.as_deref(),
            fit.as_deref(),
            &options,
        )
        .map(|_| ()),
        Command::Flash {
            release,
            options,
            probe_run,
            chip,
            probe,
            base_address,
        } => flash(
            manifest,
            release,
            &options,
            probe_run,
            chip.as_deref(),
            probe.as_deref(),
            base_address.as_deref(),
        ),
    }
}

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

fn build(
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

fn assemble(
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
        &options.payload_files,
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

/// Put the assembled FFS and the reset unit into the SP3 vendor ROM's half;
/// a 32 MiB vendor ROM also yields the dual image with only this half
/// replaced. Relative vendor paths are workspace-relative. Returns the
/// dual image when there is one, else the half.
fn compose_sp3_flash(
    workspace_root: &std::path::Path,
    image: &fstart_image_build::sp3_flash::Sp3FlashAssembly,
    assembled: &crate::assemble::AssembledImage,
) -> Result<std::path::PathBuf, String> {
    let read = |path: &std::path::Path| {
        std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
    };
    let write = |path: &std::path::Path, bytes: &[u8]| {
        std::fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
    };
    let vendor = read(&workspace_root.join(&image.vendor_image))?;
    let ffs = read(&assembled.image)?;
    let reset = assembled
        .stages
        .iter()
        .find(|stage| stage.name == image.reset_unit)
        .ok_or("SP3 reset unit was not built")?;
    let flash = fstart_image_build::sp3_flash::compose(
        &vendor,
        image.half,
        &image.compose,
        &read(&reset.run_path)?,
        &ffs,
    )?;
    if !flash.patched.locator {
        eprintln!(
            "[fstart] warning: the SP3 reset unit links no FFS locator; it cannot read its FFS"
        );
    }
    let half = assembled.image.with_extension("rom");
    write(&half, &flash.half)?;
    eprintln!(
        "[fstart] SP3 {:?} half: {} (FFS {:#x} bytes at {:#x}, reset image {:#x} bytes at {:#x})",
        image.half,
        half.display(),
        ffs.len(),
        image.compose.ffs_offset,
        image.compose.reset.size,
        image.compose.reset.flash_offset,
    );
    let Some(dual) = flash.dual else {
        return Ok(half);
    };
    let stem = half
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or("non-UTF8 image name")?;
    let out = half.with_file_name(format!("{stem}-32m.rom"));
    write(&out, &dual)?;
    eprintln!(
        "[fstart] SP3 32 MiB image: {} (other half kept from {})",
        out.display(),
        image.vendor_image
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
    payload_files: &[(String, String)],
) -> Result<crate::assemble::AssembledImage, String> {
    crate::assemble::assemble_with_parsed(
        workspace_root,
        manifest,
        parsed,
        release,
        kernel,
        firmware,
        fit,
        payload_files,
    )
}

fn flash(
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
            &options.payload_files,
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

fn run(
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
            &options.payload_files,
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
