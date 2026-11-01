use clap::{Parser, Subcommand};
use fbuild::{
    board_manifest::{self, BoardManifest},
    board_tool::{self, FlashArgs, ImageArgs, RunArgs},
    build_board,
    payload::{BuildArgs, PayloadChoice},
    resolved_image::ResolvedImage,
};
use fstart_image_build::plan::BuildSelection;

#[derive(Parser)]
#[command(name = "fbuild", about = "fstart firmware build orchestrator")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Args)]
struct BuildOptions {
    #[arg(short, long)]
    board: String,
    #[arg(short, long, default_value_t = false)]
    release: bool,
    #[arg(long, value_enum)]
    payload: Option<PayloadChoice>,
    #[command(flatten)]
    options: BuildArgs,
}

#[derive(Subcommand)]
enum Command {
    /// Run an editor's recorded Cargo check under checkout ownership.
    #[command(hide = true)]
    EditorCheck {
        #[arg(long)]
        selection: std::path::PathBuf,
    },
    Build {
        /// Build one named unit and its producers instead of the complete image.
        #[arg(long)]
        stage: Option<String>,
        #[command(flatten)]
        build: BuildOptions,
    },
    Run {
        #[command(flatten)]
        build: BuildOptions,
        #[command(flatten)]
        run: RunArgs,
    },
    Test {
        #[arg(short, long)]
        board: String,
        #[command(flatten)]
        options: BuildArgs,
    },
    Assemble {
        #[command(flatten)]
        build: BuildOptions,
        #[command(flatten)]
        image: ImageArgs,
    },
    Inspect {
        #[arg(short, long)]
        image: String,
    },
    /// Extract a board's vendor firmware blobs from a vendor flash image.
    Blobs {
        #[arg(short, long)]
        board: String,
        #[arg(long)]
        from: String,
        #[arg(long)]
        out: String,
        #[arg(long)]
        allow_missing: bool,
    },
    /// Print the resolved geometry and compiler selection.
    Explain {
        board: String,
        #[arg(long, value_enum)]
        payload: Option<PayloadChoice>,
        #[command(flatten)]
        options: BuildArgs,
    },
    /// Type-check using the resolved firmware selection.
    Check {
        board: String,
        #[arg(long)]
        stage: Option<String>,
        #[arg(long, value_enum)]
        payload: Option<PayloadChoice>,
        #[command(flatten)]
        options: BuildArgs,
        #[arg(long)]
        release: bool,
    },
    /// Generate an editor workspace for a resolved firmware selection.
    Ide {
        board: String,
        #[arg(long, value_enum)]
        payload: Option<PayloadChoice>,
        #[command(flatten)]
        options: BuildArgs,
        #[arg(long)]
        release: bool,
        #[arg(long)]
        stage: Option<String>,
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
        #[command(flatten)]
        flash: FlashArgs,
    },
}

fn board(board: &str) -> Result<BoardManifest, String> {
    board_manifest::find(&build_board::workspace_root_pub()?, board)
}

fn resolved(
    board: &str,
    selection: BuildSelection,
) -> Result<(std::path::PathBuf, BoardManifest, ResolvedImage), String> {
    let root = build_board::workspace_root_pub()?;
    let manifest = board_manifest::find(&root, board)?;
    let resolved = ResolvedImage::load(&root, &manifest, selection)?;
    Ok((root, manifest, resolved))
}

fn dispatch(command: Command) -> Result<(), String> {
    match command {
        Command::EditorCheck { selection } => fbuild::ide::check(&selection),
        Command::Build { stage, build } => {
            let manifest = board(&build.board)?;
            if let Some(stage) = stage {
                let root = build_board::workspace_root_pub()?;
                ResolvedImage::load(&root, &manifest, build.options.selection(build.payload))?
                    .compile_selected(&root, &manifest, &stage, build.release, false)
            } else {
                board_tool::build(&manifest, build.release, build.payload, &build.options)
                    .map(|_| ())
            }
        }
        Command::Run { build, run } => board_tool::run(
            &board(&build.board)?,
            build.release,
            build.payload,
            &run,
            &build.options,
        ),
        Command::Test { board: id, options } => {
            board_tool::run(&board(&id)?, true, None, &RunArgs::default(), &options)
        }
        Command::Assemble { build, image } => board_tool::assemble(
            &board(&build.board)?,
            build.release,
            build.payload,
            &image,
            &build.options,
        )
        .map(|_| ()),
        Command::Inspect { image } => fstart_image_build::inspect::inspect(&image),
        Command::Blobs {
            board: id,
            from,
            out,
            allow_missing,
        } => fbuild::blobs::extract(
            &build_board::workspace_root_pub()?,
            &board(&id)?,
            std::path::Path::new(&from),
            std::path::Path::new(&out),
            allow_missing,
        ),
        Command::Explain {
            board,
            payload,
            options,
        } => {
            println!(
                "{}",
                resolved(&board, options.selection(payload))?.2.json()?
            );
            Ok(())
        }
        Command::Check {
            board,
            payload,
            options,
            release,
            stage,
        } => {
            let (root, manifest, resolved) = resolved(&board, options.selection(payload))?;
            match stage {
                Some(stage) => resolved.compile_selected(&root, &manifest, &stage, release, true),
                None => resolved.check(&root, &manifest, release),
            }
        }
        Command::Ide {
            board,
            payload,
            options,
            release,
            stage,
            audit_lock,
        } => {
            let (root, manifest, resolved) = resolved(&board, options.selection(payload))?;
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
        Command::Flash {
            board: id,
            release,
            options,
            flash,
        } => board_tool::flash(&board(&id)?, release, &options, &flash),
    }
}

fn main() {
    if let Err(error) = dispatch(Cli::parse().command) {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn command_schema_is_consistent() {
        Cli::command().debug_assert();
    }
    use clap::CommandFactory;
    #[test]
    fn image_arguments_remain_typed_and_preserve_paths() {
        let cli = Cli::try_parse_from([
            "fbuild",
            "assemble",
            "-b",
            "qemu-riscv64",
            "--kernel",
            "a path/kernel",
            "--input",
            "dtb=a path/board.dtb",
            "--payload",
            "linux",
        ])
        .unwrap();
        let Command::Assemble { build, image } = cli.command else {
            panic!("wrong command")
        };
        assert_eq!(build.board, "qemu-riscv64");
        assert_eq!(image.kernel.as_deref(), Some("a path/kernel"));
        assert_eq!(image.inputs, [("dtb".into(), "a path/board.dtb".into())]);
    }
}
