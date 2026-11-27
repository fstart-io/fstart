use crate::payload::{AssembleArgs, BuildArgs, PayloadChoice, PlanArgs};
use crate::plan_executor::{AssembleRequest, Resolved};
use fstart_image_build::build_plan::FfsAssembly;

/// The FFS payload inputs' dedicated flags.
fn payload_aliases<'a>(
    kernel: Option<&'a str>,
    firmware: Option<&'a str>,
    fit: Option<&'a str>,
) -> [(&'static str, Option<&'a str>); 3] {
    [
        (FfsAssembly::KERNEL, kernel),
        (FfsAssembly::FIRMWARE, firmware),
        (FfsAssembly::FIT, fit),
    ]
}

fn load(
    manifest: &crate::board_manifest::BoardManifest,
    payload: Option<PayloadChoice>,
    options: &PlanArgs,
) -> Result<Resolved, String> {
    let root = crate::build_board::workspace_root()?;
    Resolved::load(&root, manifest, options.selection(payload))
}

/// Run board policy and protocol tests with the board's default and the
/// selected variant's features. Firmware build selections remain separate: a
/// host test must not inherit a stage's linker script, target or generated
/// artifact bindings.
pub fn test_library(manifest: &crate::board_manifest::BoardManifest) -> Result<(), String> {
    let root = crate::build_board::workspace_root()?;
    let host = crate::build_board::host_target(&root)?;
    let mut command = std::process::Command::new("cargo");
    command
        .current_dir(&root)
        .args(["test", "--locked", "--lib", "--manifest-path"])
        .arg(root.join("Cargo.toml"))
        .args(["--package", &manifest.package, "--target", &host])
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env("RUSTFLAGS", "");
    if !manifest.variant_features.is_empty() {
        command.args(["--features", &manifest.variant_features.join(",")]);
    }
    let target_lock = crate::target_dir_lock(&root.join("target"));
    let _target_guard = target_lock.lock().unwrap_or_else(|e| e.into_inner());
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
    options: &PlanArgs,
) -> Result<crate::build_board::BuildResult, String> {
    let root = crate::build_board::workspace_root()?;
    let resolved = Resolved::load(&root, manifest, options.selection(payload))?;
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
    let workspace_root = crate::build_board::workspace_root()?;
    let resolved = load(manifest, payload, &options.plan)?;
    let supplied = crate::assemble::supplied_inputs(
        &payload_aliases(kernel, firmware, fit),
        &options.assemble.inputs,
    )?;
    let settings =
        crate::payload::store_settings(&options.assemble.settings, &options.assemble.secure_boot)?;
    Ok(resolved
        .assemble(
            &workspace_root,
            manifest,
            &AssembleRequest {
                release,
                supplied: &supplied,
                payload_files: &options.assemble.payload_files,
                signing_key: options.assemble.signing_key.as_deref(),
                settings: &settings,
            },
        )?
        .image)
}

#[allow(clippy::too_many_arguments)] // the CLI's flat flash options
pub fn flash(
    manifest: &crate::board_manifest::BoardManifest,
    release: bool,
    options: &BuildArgs,
    probe_run: bool,
    chip: Option<&str>,
    probe_selector: Option<&str>,
    protocol: &str,
    base_address: Option<&str>,
) -> Result<(), String> {
    let workspace_root = crate::build_board::workspace_root()?;
    let resolved = load(manifest, None, &options.plan)?;
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
            .arg(protocol);
        if let Some(sel) = probe_selector {
            cmd.arg("--probe").arg(sel);
        }
        cmd
    };

    if !probe_run {
        // Default to the firmware window the board's plan states; an
        // explicit --base-address wins.
        let plan = &resolved.plan;
        let base_address = match base_address {
            Some(base) => base.to_string(),
            None => match plan.ffs_assembly().map(|ffs| ffs.build.firmware_image) {
                Ok(fstart_core::FirmwareImagePolicy::MemoryMapped { cpu_base, .. }) => {
                    format!("{cpu_base:#x}")
                }
                _ => {
                    return Err("flash requires --base-address: the board's plan states no \
                         memory-mapped firmware window"
                        .into());
                }
            },
        };
        let base_address = base_address.as_str();
        eprintln!("[fstart] step 1/2: assembling FFS and flashing target memory...");
        let supplied = crate::assemble::supplied_inputs(&[], &options.assemble.inputs)?;
        let settings = crate::payload::store_settings(
            &options.assemble.settings,
            &options.assemble.secure_boot,
        )?;
        let mut assembled = resolved.assemble(
            &workspace_root,
            &manifest,
            &AssembleRequest {
                release,
                supplied: &supplied,
                payload_files: &options.assemble.payload_files,
                signing_key: options.assemble.signing_key.as_deref(),
                settings: &settings,
            },
        )?;
        // The FFS goes to the firmware window; a plan without one flashes
        // its final image there.
        let ffs_path = plan
            .ffs_step()
            .and_then(|(step, _)| assembled.artifacts.remove(step))
            .unwrap_or(assembled.image);

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
    which_in_path("probe-rs").ok_or_else(|| "not in PATH".to_string())
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

/// A built image ready to boot as its plan's run recipe describes.
pub struct QemuImage {
    pub recipe: fstart_image_build::run::RunRecipe,
    pub image: crate::assemble::AssembledImage,
    /// The recipe's external media, by input name.
    pub external: std::collections::BTreeMap<String, std::path::PathBuf>,
}

/// Build and assemble the image `resolved` selects, binding `supplied` to
/// the plan's inputs and the run recipe's external media: what `fbuild
/// qemu` and the boot tests launch.
pub fn qemu_image(
    manifest: &crate::board_manifest::BoardManifest,
    resolved: &Resolved,
    release: bool,
    mut supplied: std::collections::BTreeMap<String, std::path::PathBuf>,
    options: &AssembleArgs,
) -> Result<QemuImage, String> {
    let workspace_root = crate::build_board::workspace_root()?;
    // The plan states how (and whether) this board runs under QEMU; there
    // is no per-architecture fallback launch to guess.
    let recipe = resolved.plan.run.clone().ok_or_else(|| {
        format!(
            "board '{}' has no QEMU run recipe in its plan",
            manifest.board
        )
    })?;
    // `--secure-firmware` and `--input` bind the run recipe's external
    // media and the plan's inputs from one namespace.
    let external: std::collections::BTreeMap<_, _> = recipe
        .external_inputs()
        .filter_map(|name| supplied.remove_entry(name))
        .collect();
    let image = if resolved.plan.images.is_empty() {
        if let Some(name) = supplied.keys().next() {
            return Err(format!("input {name} is not declared by the selected plan"));
        }
        let build = resolved.build(&workspace_root, manifest, release)?;
        crate::assemble::AssembledImage {
            image: build.primary_binary().run_path.clone(),
            artifacts: Default::default(),
            stages: build.stages,
        }
    } else {
        let settings = crate::payload::store_settings(&options.settings, &options.secure_boot)?;
        resolved.assemble(
            &workspace_root,
            manifest,
            &AssembleRequest {
                release,
                supplied: &supplied,
                payload_files: &options.payload_files,
                signing_key: options.signing_key.as_deref(),
                settings: &settings,
            },
        )?
    };
    Ok(QemuImage {
        recipe,
        image,
        external,
    })
}

/// The `fbuild qemu` options beyond the build selection.
#[derive(Debug, Default)]
pub struct QemuArgs<'a> {
    pub kernel: Option<&'a str>,
    pub firmware: Option<&'a str>,
    pub fit: Option<&'a str>,
    pub disk: Option<&'a str>,
    pub memory: Option<&'a str>,
    pub secure_firmware: Option<&'a str>,
    pub tpm: Option<fstart_image_build::boot_test::Tpm>,
}

pub fn qemu(
    manifest: &crate::board_manifest::BoardManifest,
    release: bool,
    payload: Option<PayloadChoice>,
    args: &QemuArgs<'_>,
    options: &BuildArgs,
) -> Result<(), String> {
    let resolved = load(manifest, payload, &options.plan)?;
    let supplied = crate::assemble::supplied_inputs(
        &[
            payload_aliases(args.kernel, args.firmware, args.fit).as_slice(),
            &[("secure-firmware", args.secure_firmware)],
        ]
        .concat(),
        &options.assemble.inputs,
    )?;
    let built = qemu_image(manifest, &resolved, release, supplied, &options.assemble)?;
    let tpm = args
        .tpm
        .as_ref()
        .map(crate::swtpm::Swtpm::start)
        .transpose()
        .map_err(|e| format!("swtpm: {e}"))?;
    let tpm_socket = tpm.as_ref().map(crate::swtpm::Swtpm::socket);
    // The environment carries a developer's per-run overrides: the vCPU
    // count for MP/SMM flight plans, and extra devices.
    let smp = std::env::var("FSTART_QEMU_SMP").ok();
    let launch = crate::qemu::Launch {
        disk: args.disk,
        memory: args.memory,
        smp: smp.as_deref(),
        tpm: tpm_socket.as_deref(),
        args: std::env::var("FSTART_QEMU_EXTRA_ARGS")
            .map(|extra| extra.split_whitespace().map(str::to_string).collect())
            .unwrap_or_default(),
    };
    crate::qemu::run(crate::qemu::command(
        &built.recipe,
        &built.image,
        &built.external,
        &launch,
    )?)
}
