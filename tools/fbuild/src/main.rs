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
        /// Append the stage sizes as one JSON line to this file (CI size report).
        #[arg(long)]
        size_report: Option<std::path::PathBuf>,
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
    /// Extract a board's vendor firmware blobs (AMD PSP firmware, APCBs,
    /// microcode, video BIOS) from a vendor flash image into a blob
    /// directory for `--blobs`.
    Blobs {
        #[arg(short, long)]
        board: String,
        /// The vendor flash image.
        #[arg(long)]
        from: String,
        /// The blob directory to write.
        #[arg(long)]
        out: String,
        /// Write what the image has even if some files are missing.
        #[arg(long)]
        allow_missing: bool,
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

/// Resolve the board package the requested build selects.
fn manifest_for(board: &str) -> Result<board_manifest::BoardManifest, String> {
    let root = build_board::workspace_root_pub()?;
    let manifest = board_manifest::find(&root, board)?;
    if manifest.build_profile.is_none() {
        return Err(format!(
            "board '{board}' has no build-profile; legacy host tools are retired"
        ));
    }
    Ok(manifest)
}

/// Resolve the size report path against the caller's cwd before the build
/// writes artifacts elsewhere.
fn absolute_size_report(
    path: Option<std::path::PathBuf>,
) -> Result<Option<std::path::PathBuf>, String> {
    path.map(|path| {
        std::path::absolute(&path).map_err(|e| format!("--size-report {}: {e}", path.display()))
    })
    .transpose()
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
            size_report,
        } => match (stage, size_report) {
            (Some(_), Some(_)) => Err("--size-report needs a complete build, not --stage".into()),
            (Some(name), None) => {
                compile_named(&board, options.selection(payload), release, &name, false)
            }
            (None, size_report) => manifest_for(&board).and_then(|manifest| {
                let build = fbuild::board_tool::build(&manifest, release, payload, &options)?;
                match absolute_size_report(size_report)? {
                    Some(path) => fbuild::sizes::append(&path, &manifest.board, &build.stages),
                    None => Ok(()),
                }
            }),
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
        } => manifest_for(&board).and_then(|manifest| {
            fbuild::board_tool::run(
                &manifest,
                release,
                payload,
                kernel.as_deref(),
                firmware.as_deref(),
                fit.as_deref(),
                disk.as_deref(),
                memory.as_deref(),
                secure_firmware.as_deref(),
                &options,
            )
        }),
        Command::Test { board, options } => manifest_for(&board).and_then(|manifest| {
            fbuild::board_tool::run(
                &manifest, true, None, None, None, None, None, None, None, &options,
            )
        }),
        Command::Assemble {
            board,
            release,
            payload,
            options,
            kernel,
            firmware,
            fit,
        } => manifest_for(&board).and_then(|manifest| {
            fbuild::board_tool::assemble(
                &manifest,
                release,
                payload,
                kernel.as_deref(),
                firmware.as_deref(),
                fit.as_deref(),
                &options,
            )
            .map(|_| ())
        }),
        Command::Inspect { image } => fstart_image_build::inspect::inspect(&image),
        Command::Blobs {
            board,
            from,
            out,
            allow_missing,
        } => build_board::workspace_root_pub().and_then(|root| {
            let manifest = board_manifest::find(&root, &board)?;
            fbuild::blobs::extract(
                &root,
                &manifest,
                std::path::Path::new(&from),
                std::path::Path::new(&out),
                allow_missing,
            )
        }),
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
        } => manifest_for(&board).and_then(|manifest| {
            fbuild::board_tool::flash(
                &manifest,
                release,
                &options,
                probe_run,
                chip.as_deref(),
                probe.as_deref(),
                base_address.as_deref(),
            )
        }),
    };

    if let Err(e) = result {
        eprintln!("error: {e}");
        process::exit(1);
    }
}
