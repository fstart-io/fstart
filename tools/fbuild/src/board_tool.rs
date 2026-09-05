use clap::{Parser, Subcommand};
use fstart_core::acpi::AcpiExtraDevice;
use fstart_core::{BoardConfig, StageLayout};

use crate::build_plan::ParsedBoard;
use crate::payload::{PayloadChoice, apply_payload_override};

#[derive(Clone, Copy)]
pub struct BoardCallbacks {
    pub board_config: fn() -> BoardConfig,
    pub acpi_only_devices: Option<fn() -> Vec<AcpiExtraDevice>>,
}

#[derive(Parser)]
#[command(name = "fstart-board-tool", about = "board-owned fstart build tool")]
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
    },
    Run {
        #[arg(short, long, default_value_t = false)]
        release: bool,
        #[arg(long, value_enum)]
        payload: Option<PayloadChoice>,
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
    Test,
    Assemble {
        #[arg(short, long, default_value_t = false)]
        release: bool,
        #[arg(long, value_enum)]
        payload: Option<PayloadChoice>,
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

#[macro_export]
macro_rules! board_host_tool {
    ($board_config:path) => {
        fn main() {
            fbuild::board_tool::main(fbuild::board_tool::BoardCallbacks {
                board_config: $board_config,
                acpi_only_devices: None,
            });
        }
    };
    ($board_config:path, acpi_only_devices = $acpi_only_devices:path) => {
        fn main() {
            fbuild::board_tool::main(fbuild::board_tool::BoardCallbacks {
                board_config: $board_config,
                acpi_only_devices: Some($acpi_only_devices),
            });
        }
    };
}

pub fn main(callbacks: BoardCallbacks) {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Build { release, payload } => build(callbacks, release, payload).map(|_| ()),
        Command::Run {
            release,
            payload,
            kernel,
            firmware,
            fit,
            disk,
            memory,
            secure_firmware,
        } => run(
            callbacks,
            release,
            payload,
            kernel.as_deref(),
            firmware.as_deref(),
            fit.as_deref(),
            disk.as_deref(),
            memory.as_deref(),
            secure_firmware.as_deref(),
        ),
        Command::Test => run(callbacks, true, None, None, None, None, None, None, None),
        Command::Assemble {
            release,
            payload,
            kernel,
            firmware,
            fit,
        } => assemble(
            callbacks,
            release,
            payload,
            kernel.as_deref(),
            firmware.as_deref(),
            fit.as_deref(),
        )
        .map(|_| ()),
        Command::Flash {
            release,
            probe_run,
            chip,
            probe,
            base_address,
        } => flash(
            callbacks,
            release,
            probe_run,
            chip.as_deref(),
            probe.as_deref(),
            base_address.as_deref(),
        ),
    };

    if let Err(err) = result {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn load(
    callbacks: BoardCallbacks,
    payload: Option<PayloadChoice>,
) -> Result<(crate::board_manifest::BoardManifest, ParsedBoard), String> {
    let mut config = (callbacks.board_config)();
    apply_payload_override(&mut config, payload)?;
    config
        .memory
        .normalize_derived_flash()
        .map_err(|err| err.to_string())?;
    let acpi_only_devices = callbacks
        .acpi_only_devices
        .map_or_else(Vec::new, |load| load());
    let workspace_root = crate::build_board::workspace_root_pub()?;
    let manifest = crate::board_manifest::find(&workspace_root, config.name.as_str())?;
    validate(&manifest, &config)?;
    Ok((
        manifest,
        ParsedBoard {
            config,
            acpi_only_devices,
        },
    ))
}

fn build(
    callbacks: BoardCallbacks,
    release: bool,
    payload: Option<PayloadChoice>,
) -> Result<crate::build_board::BuildResult, String> {
    let workspace_root = crate::build_board::workspace_root_pub()?;
    let (manifest, parsed) = load(callbacks, payload)?;
    crate::build_board::build_with_parsed(&workspace_root, &manifest, &parsed, release)
}

fn assemble(
    callbacks: BoardCallbacks,
    release: bool,
    payload: Option<PayloadChoice>,
    kernel: Option<&str>,
    firmware: Option<&str>,
    fit: Option<&str>,
) -> Result<std::path::PathBuf, String> {
    let workspace_root = crate::build_board::workspace_root_pub()?;
    let (manifest, parsed) = load(callbacks, payload)?;
    assemble_loaded(
        &workspace_root,
        manifest,
        parsed,
        release,
        kernel,
        firmware,
        fit,
    )
}

fn assemble_loaded(
    workspace_root: &std::path::Path,
    manifest: crate::board_manifest::BoardManifest,
    parsed: ParsedBoard,
    release: bool,
    kernel: Option<&str>,
    firmware: Option<&str>,
    fit: Option<&str>,
) -> Result<std::path::PathBuf, String> {
    crate::assemble::assemble_with_parsed(
        workspace_root,
        manifest,
        parsed,
        release,
        kernel,
        firmware,
        fit,
    )
}

fn flash(
    callbacks: BoardCallbacks,
    release: bool,
    probe_run: bool,
    chip: Option<&str>,
    probe_selector: Option<&str>,
    base_address: Option<&str>,
) -> Result<(), String> {
    let workspace_root = crate::build_board::workspace_root_pub()?;
    let (manifest, parsed) = load(callbacks, None)?;
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
        )?;

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
    callbacks: BoardCallbacks,
    release: bool,
    payload: Option<PayloadChoice>,
    kernel: Option<&str>,
    firmware: Option<&str>,
    fit: Option<&str>,
    disk: Option<&str>,
    memory: Option<&str>,
    secure_firmware: Option<&str>,
) -> Result<(), String> {
    let workspace_root = crate::build_board::workspace_root_pub()?;
    let (manifest, parsed) = load(callbacks, payload)?;
    let config = &parsed.config;
    let build_policy = config.build.clone();
    let platform = config.platform;
    let is_multi_stage = matches!(config.stages, StageLayout::MultiStage(_));
    let needs_x86_pflash = matches!(config.platform, fstart_core::Platform::X86_64);
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
        let image_path = assemble_loaded(
            &workspace_root,
            manifest,
            parsed,
            release,
            kernel,
            firmware,
            fit,
        )?;
        crate::qemu::run(
            &build_policy,
            platform,
            &image_path,
            disk,
            memory,
            secure_firmware,
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
        )
    }
}

fn validate(
    manifest: &crate::board_manifest::BoardManifest,
    config: &BoardConfig,
) -> Result<(), String> {
    if config.name.as_str() != manifest.board {
        return Err(format!(
            "board config name mismatch for {}: manifest board is '{}', board returned '{}'",
            manifest.package, manifest.board, config.name
        ));
    }
    if let Some(platform) = &manifest.platform
        && config.platform.as_str() != platform
    {
        return Err(format!(
            "board platform mismatch for {}: manifest platform is '{}', board returned '{}'",
            manifest.package, platform, config.platform
        ));
    }
    if let Some(target) = &manifest.target
        && config.platform.target_triple() != target
    {
        return Err(format!(
            "board target mismatch for {}: manifest target is '{}', platform implies '{}'",
            manifest.package,
            target,
            config.platform.target_triple()
        ));
    }
    Ok(())
}
