use clap::{Parser, Subcommand};
use fbuild::{board_manifest, build_board, payload::PayloadChoice};
use std::process;

#[derive(Parser)]
#[command(name = "fbuild", about = "fstart firmware build orchestrator")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Build {
        #[arg(short, long)]
        board: String,
        #[arg(short, long, default_value_t = false)]
        release: bool,
        #[arg(long, value_enum)]
        payload: Option<PayloadChoice>,
    },
    Run {
        #[arg(short, long)]
        board: String,
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
    Test {
        #[arg(short, long)]
        board: String,
    },
    Assemble {
        #[arg(short, long)]
        board: String,
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
    Inspect {
        #[arg(short, long)]
        image: String,
    },
    /// Explain a migrated board's resolved geometry and compiler selection.
    Explain {
        board: String,
        #[arg(long, value_enum)]
        payload: Option<PayloadChoice>,
    },
    /// Type-check a migrated board using its resolved firmware selection.
    Check {
        board: String,
        #[arg(long, value_enum)]
        payload: Option<PayloadChoice>,
        #[arg(long)]
        release: bool,
    },
    /// Generate an opt-in editor workspace for a resolved firmware selection.
    Ide {
        board: String,
        #[arg(long, value_enum)]
        payload: Option<PayloadChoice>,
        #[arg(long)]
        release: bool,
        /// Select exactly one Intel compiler role: bootblock, postcar, ramstage.
        #[arg(long)]
        stage: Option<fbuild::intel_layout::IntelStage>,
        /// Compare against a disposable all-board build-lock prototype.
        #[arg(long)]
        audit_lock: bool,
    },
    Flash {
        #[arg(short, long)]
        board: String,
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

fn dispatch_board_host(board: &str, args: &[String]) -> Result<(), String> {
    let workspace_root = build_board::workspace_root_pub()?;
    let manifest = board_manifest::find(&workspace_root, board)?;
    if manifest.build_profile.is_some() {
        return fbuild::board_tool::run_metadata(&manifest, args);
    }
    let selected_workspace =
        build_board::prepare_selected_board_workspace(&workspace_root, &manifest)?;
    let mut host_features = String::from("host");
    for feature in &manifest.variant_features {
        host_features.push(',');
        host_features.push_str(feature);
    }
    let status = std::process::Command::new("cargo")
        .current_dir(&workspace_root)
        .arg("run")
        .arg("--quiet")
        .arg("--manifest-path")
        .arg(selected_workspace.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(workspace_root.join("target"))
        .arg("--package")
        .arg(&manifest.package)
        .arg("--bin")
        .arg("fstart-host")
        .arg("--no-default-features")
        .arg("--features")
        .arg(&host_features)
        .arg("--")
        .args(args)
        .env("FSTART_WORKSPACE_ROOT", &workspace_root)
        .env("FSTART_BOARD_VARIANT", &manifest.board)
        .status()
        .map_err(|e| format!("failed to run host tool for {}: {e}", manifest.board))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "host tool for '{}' failed with {status}",
            manifest.board
        ))
    }
}

fn board_tool_build_args(
    subcommand: &str,
    release: bool,
    payload: Option<PayloadChoice>,
) -> Vec<String> {
    let mut args = vec![subcommand.to_string()];
    if release {
        args.push("--release".to_string());
    }
    if let Some(payload) = payload {
        args.push("--payload".to_string());
        args.push(payload.as_str().to_string());
    }
    args
}

fn board_tool_assemble_args(
    subcommand: &str,
    release: bool,
    payload: Option<PayloadChoice>,
    kernel: Option<String>,
    firmware: Option<String>,
    fit: Option<String>,
) -> Vec<String> {
    let mut args = board_tool_build_args(subcommand, release, payload);
    if let Some(kernel) = kernel {
        args.push("--kernel".to_string());
        args.push(kernel);
    }
    if let Some(firmware) = firmware {
        args.push("--firmware".to_string());
        args.push(firmware);
    }
    if let Some(fit) = fit {
        args.push("--fit".to_string());
        args.push(fit);
    }
    args
}

fn board_tool_run_args(
    subcommand: &str,
    release: bool,
    payload: Option<PayloadChoice>,
    kernel: Option<String>,
    firmware: Option<String>,
    fit: Option<String>,
    disk: Option<String>,
    memory: Option<String>,
    secure_firmware: Option<String>,
) -> Vec<String> {
    let mut args = board_tool_assemble_args(subcommand, release, payload, kernel, firmware, fit);
    if let Some(disk) = disk {
        args.push("--disk".to_string());
        args.push(disk);
    }
    if let Some(memory) = memory {
        args.push("--memory".to_string());
        args.push(memory);
    }
    if let Some(secure_firmware) = secure_firmware {
        args.push("--secure-firmware".to_string());
        args.push(secure_firmware);
    }
    args
}

fn board_tool_flash_args(
    release: bool,
    probe_run: bool,
    chip: Option<String>,
    probe: Option<String>,
    base_address: Option<String>,
) -> Vec<String> {
    let mut args = board_tool_build_args("flash", release, None);
    if probe_run {
        args.push("--probe-run".to_string());
    }
    if let Some(chip) = chip {
        args.push("--chip".to_string());
        args.push(chip);
    }
    if let Some(probe) = probe {
        args.push("--probe".to_string());
        args.push(probe);
    }
    if let Some(base_address) = base_address {
        args.push("--base-address".to_string());
        args.push(base_address);
    }
    args
}

fn explain(board: &str, payload: Option<PayloadChoice>) -> Result<(), String> {
    let root = build_board::workspace_root_pub()?;
    let manifest = board_manifest::find(&root, board)?;
    let resolved = fbuild::resolved_image::ResolvedImage::load(&root, &manifest, payload)?;
    println!("{}", resolved.json()?);
    Ok(())
}

fn check(board: &str, payload: Option<PayloadChoice>, release: bool) -> Result<(), String> {
    let root = build_board::workspace_root_pub()?;
    let manifest = board_manifest::find(&root, board)?;
    let resolved = fbuild::resolved_image::ResolvedImage::load(&root, &manifest, payload)?;
    resolved.check(&root, &manifest, release)
}

fn ide(
    board: &str,
    payload: Option<PayloadChoice>,
    release: bool,
    audit_lock: bool,
    stage: Option<fbuild::intel_layout::IntelStage>,
) -> Result<(), String> {
    let root = build_board::workspace_root_pub()?;
    let manifest = board_manifest::find(&root, board)?;
    let resolved = fbuild::resolved_image::ResolvedImage::load(&root, &manifest, payload)?;
    let workspace =
        fbuild::ide::generate_image(&root, &manifest, &resolved, stage, release, audit_lock)?;
    println!("{}", workspace.display());
    Ok(())
}

fn main() {
    let cli = Cli::parse();

    let result: Result<(), String> = match cli.command {
        Command::Build {
            board,
            release,
            payload,
        } => dispatch_board_host(&board, &board_tool_build_args("build", release, payload)),
        Command::Run {
            board,
            release,
            payload,
            kernel,
            firmware,
            fit,
            disk,
            memory,
            secure_firmware,
        } => dispatch_board_host(
            &board,
            &board_tool_run_args(
                "run",
                release,
                payload,
                kernel,
                firmware,
                fit,
                disk,
                memory,
                secure_firmware,
            ),
        ),
        Command::Test { board } => dispatch_board_host(&board, &["test".into()]),
        Command::Assemble {
            board,
            release,
            payload,
            kernel,
            firmware,
            fit,
        } => dispatch_board_host(
            &board,
            &board_tool_assemble_args("assemble", release, payload, kernel, firmware, fit),
        ),
        Command::Inspect { image } => fstart_image_build::inspect::inspect(&image),
        Command::Explain { board, payload } => explain(&board, payload),
        Command::Ide {
            board,
            payload,
            release,
            audit_lock,
            stage,
        } => ide(&board, payload, release, audit_lock, stage),
        Command::Check {
            board,
            payload,
            release,
        } => check(&board, payload, release),
        Command::Flash {
            board,
            release,
            probe_run,
            chip,
            probe,
            base_address,
        } => dispatch_board_host(
            &board,
            &board_tool_flash_args(release, probe_run, chip, probe, base_address),
        ),
    };

    if let Err(e) = result {
        eprintln!("error: {e}");
        process::exit(1);
    }
}
