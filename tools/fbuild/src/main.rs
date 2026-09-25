use clap::{Parser, Subcommand};
use fbuild::{
    board_manifest, build_board,
    payload::{BuildArgs, PayloadChoice},
};
use fstart_image_build::plan::BuildSelection;
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
        /// Build one named unit and its producers instead of the complete image.
        #[arg(long)]
        stage: Option<String>,
        #[arg(short, long)]
        board: String,
        #[arg(short, long, default_value_t = false)]
        release: bool,
        #[arg(long, value_enum)]
        payload: Option<PayloadChoice>,
        #[command(flatten)]
        options: BuildArgs,
    },
    Run {
        #[arg(short, long)]
        board: String,
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
        #[arg(short, long)]
        board: String,
        #[command(flatten)]
        options: BuildArgs,
    },
    Assemble {
        #[arg(short, long)]
        board: String,
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
    Inspect {
        #[arg(short, long)]
        image: String,
    },
    /// Explain a migrated board's resolved geometry and compiler selection.
    Explain {
        board: String,
        #[arg(long, value_enum)]
        payload: Option<PayloadChoice>,
        #[command(flatten)]
        options: BuildArgs,
    },
    /// Type-check a migrated board using its resolved firmware selection.
    Check {
        board: String,
        /// Check one named unit after building its producer closure.
        #[arg(long)]
        stage: Option<String>,
        #[arg(long, value_enum)]
        payload: Option<PayloadChoice>,
        #[command(flatten)]
        options: BuildArgs,
        #[arg(long)]
        release: bool,
    },
    /// Generate an opt-in editor workspace for a resolved firmware selection.
    Ide {
        board: String,
        #[arg(long, value_enum)]
        payload: Option<PayloadChoice>,
        #[command(flatten)]
        options: BuildArgs,
        #[arg(long)]
        release: bool,
        /// Select a named compilation unit from the resolved platform plan.
        #[arg(long)]
        stage: Option<String>,
        /// Compare against a disposable all-board build-lock prototype.
        #[arg(long)]
        audit_lock: bool,
    },
    Flash {
        #[arg(short, long)]
        board: String,
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

fn dispatch_board(board: &str, args: &[String]) -> Result<(), String> {
    let workspace_root = build_board::workspace_root_pub()?;
    let manifest = board_manifest::find(&workspace_root, board)?;
    if manifest.build_profile.is_none() {
        return Err(format!(
            "board '{board}' has no build-profile; legacy host tools are retired"
        ));
    }
    fbuild::board_tool::run_metadata(&manifest, args)
}

fn board_tool_build_args(
    subcommand: &str,
    release: bool,
    payload: Option<PayloadChoice>,
    options: &BuildArgs,
) -> Vec<String> {
    let mut args = vec![subcommand.to_string()];
    if release {
        args.push("--release".to_string());
    }
    if let Some(payload) = payload {
        args.push("--payload".to_string());
        args.push(payload.as_str().to_string());
    }
    options.append_cli_args(&mut args);
    args
}

fn board_tool_assemble_args(
    subcommand: &str,
    release: bool,
    payload: Option<PayloadChoice>,
    kernel: Option<String>,
    firmware: Option<String>,
    fit: Option<String>,
    options: &BuildArgs,
) -> Vec<String> {
    let mut args = board_tool_build_args(subcommand, release, payload, options);
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

// One parameter per forwarded CLI option; each becomes its own argv entry.
#[allow(clippy::too_many_arguments)]
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
    options: &BuildArgs,
) -> Vec<String> {
    let mut args =
        board_tool_assemble_args(subcommand, release, payload, kernel, firmware, fit, options);
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
    options: &BuildArgs,
    probe_run: bool,
    chip: Option<String>,
    probe: Option<String>,
    base_address: Option<String>,
) -> Vec<String> {
    let mut args = board_tool_build_args("flash", release, None, options);
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

fn explain(board: &str, selection: BuildSelection) -> Result<(), String> {
    let root = build_board::workspace_root_pub()?;
    let manifest = board_manifest::find(&root, board)?;
    let resolved = fbuild::resolved_image::ResolvedImage::load(&root, &manifest, selection)?;
    println!("{}", resolved.json()?);
    Ok(())
}

fn compile_named(
    board: &str,
    selection: BuildSelection,
    release: bool,
    name: &str,
    checking: bool,
) -> Result<(), String> {
    let root = build_board::workspace_root_pub()?;
    let manifest = board_manifest::find(&root, board)?;
    let resolved = fbuild::resolved_image::ResolvedImage::load(&root, &manifest, selection)?;
    resolved.compile_selected(&root, &manifest, name, release, checking)
}

fn check(
    board: &str,
    selection: BuildSelection,
    release: bool,
    stage: Option<String>,
) -> Result<(), String> {
    if let Some(stage) = stage {
        return compile_named(board, selection, release, &stage, true);
    }
    let root = build_board::workspace_root_pub()?;
    let manifest = board_manifest::find(&root, board)?;
    let resolved = fbuild::resolved_image::ResolvedImage::load(&root, &manifest, selection)?;
    resolved.check(&root, &manifest, release)
}

fn ide(
    board: &str,
    selection: BuildSelection,
    release: bool,
    audit_lock: bool,
    stage: Option<String>,
) -> Result<(), String> {
    let root = build_board::workspace_root_pub()?;
    let manifest = board_manifest::find(&root, board)?;
    let resolved = fbuild::resolved_image::ResolvedImage::load(&root, &manifest, selection)?;
    let workspace = fbuild::ide::generate_image(
        &root,
        &manifest,
        &resolved,
        stage.as_deref(),
        release,
        audit_lock,
    )?;
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
            options,
            stage,
        } => match stage {
            Some(name) => compile_named(&board, options.selection(payload), release, &name, false),
            None => dispatch_board(
                &board,
                &board_tool_build_args("build", release, payload, &options),
            ),
        },
        Command::Run {
            board,
            release,
            payload,
            options,
            kernel,
            firmware,
            fit,
            disk,
            memory,
            secure_firmware,
        } => dispatch_board(
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
                &options,
            ),
        ),
        Command::Test { board, options } => dispatch_board(
            &board,
            &board_tool_build_args("test", false, None, &options),
        ),
        Command::Assemble {
            board,
            release,
            payload,
            options,
            kernel,
            firmware,
            fit,
        } => dispatch_board(
            &board,
            &board_tool_assemble_args(
                "assemble", release, payload, kernel, firmware, fit, &options,
            ),
        ),
        Command::Inspect { image } => fstart_image_build::inspect::inspect(&image),
        Command::Explain {
            board,
            payload,
            options,
        } => explain(&board, options.selection(payload)),
        Command::Ide {
            board,
            payload,
            options,
            release,
            audit_lock,
            stage,
        } => ide(
            &board,
            options.selection(payload),
            release,
            audit_lock,
            stage,
        ),
        Command::Check {
            board,
            payload,
            options,
            release,
            stage,
        } => check(&board, options.selection(payload), release, stage),
        Command::Flash {
            board,
            release,
            options,
            probe_run,
            chip,
            probe,
            base_address,
        } => dispatch_board(
            &board,
            &board_tool_flash_args(release, &options, probe_run, chip, probe, base_address),
        ),
    };

    if let Err(e) = result {
        eprintln!("error: {e}");
        process::exit(1);
    }
}
