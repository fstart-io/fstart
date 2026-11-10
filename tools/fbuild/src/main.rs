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
    /// List every board fbuild can build, sorted by board id.
    Boards {
        /// Print one JSON array of board objects instead of one id per line.
        #[arg(long)]
        json: bool,
    },
    /// Export compressed, donor-free browser assembly bundles and all unit
    /// binaries. Without --board/--payload, exports every board/variant and
    /// every payload selection its platform declares.
    Bundle {
        #[arg(long)]
        board: Option<String>,
        #[arg(long, value_enum)]
        payload: Option<PayloadChoice>,
        #[arg(long)]
        out: std::path::PathBuf,
        #[arg(long)]
        revision: String,
        /// A SeaBIOS build (ci/payloads/seabios/build.sh output) to ship in
        /// every coreboot-payload bundle.
        #[arg(long)]
        seabios: Option<std::path::PathBuf>,
    },
    /// Assemble a downloaded bundle without invoking Cargo or a linker.
    ComposeBundle {
        #[arg(long)]
        bundle: std::path::PathBuf,
        #[arg(long)]
        out: std::path::PathBuf,
        #[arg(long)]
        donor: Option<std::path::PathBuf>,
        /// Named external input, e.g. kernel=bzImage.
        #[arg(long = "input")]
        inputs: Vec<String>,
        #[arg(long)]
        signing_key: Option<std::path::PathBuf>,
    },
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
    /// Execute host library tests for one board variant, or every discovered variant.
    TestLibrary {
        #[arg(short, long)]
        board: Option<String>,
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
    /// Print the options a build offers, as JSON: the option records of its
    /// last stage, checked against the earlier ones.
    Options {
        /// The build's stage ELFs, in boot order.
        #[arg(required = true)]
        elfs: Vec<String>,
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
    /// Explain a board's resolved geometry and compiler selection.
    Explain {
        board: String,
        #[arg(long, value_enum)]
        payload: Option<PayloadChoice>,
        #[command(flatten)]
        options: BuildArgs,
    },
    /// Type-check a board using its resolved firmware selection.
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
        return Err(format!("board '{board}' has no build-profile"));
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
    stage: Option<String>,
) -> Result<(), String> {
    let root = build_board::workspace_root_pub()?;
    let manifest = board_manifest::find(&root, board)?;
    let resolved = fbuild::resolved_image::ResolvedImage::load(&root, &manifest, selection)?;
    let workspace =
        fbuild::ide::generate_image(&root, &manifest, &resolved, stage.as_deref(), release)?;
    println!("{}", workspace.display());
    Ok(())
}

fn test_libraries(board: Option<&str>) -> Result<(), String> {
    if let Some(board) = board {
        return fbuild::board_tool::test_library(&manifest_for(board)?);
    }
    let root = build_board::workspace_root_pub()?;
    for board in board_manifest::discover(&root)? {
        fbuild::board_tool::test_library(&board)?;
        for (variant, _) in &board.variants {
            fbuild::board_tool::test_library(&board_manifest::find(&root, variant)?)?;
        }
    }
    Ok(())
}

/// The `fbuild boards --json` view of one discovered board.
#[derive(serde::Serialize)]
struct BoardEntry<'a> {
    board: &'a str,
    package: &'a str,
    dir: String,
    platform: Option<&'a str>,
    target: Option<&'a str>,
    build_profile: Option<&'a board_manifest::ProfileRef>,
    variants: Vec<&'a str>,
}

fn boards(json: bool) -> Result<(), String> {
    let root = build_board::workspace_root_pub()?;
    let boards = board_manifest::discover(&root)?;
    if json {
        let entries: Vec<_> = boards
            .iter()
            .map(|board| BoardEntry {
                board: &board.board,
                package: &board.package,
                dir: board.rel_dir.display().to_string(),
                platform: board.platform.as_deref(),
                target: board.target.as_deref(),
                build_profile: board.build_profile.as_ref(),
                variants: board.variants.iter().map(|(id, _)| id.as_str()).collect(),
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&entries).map_err(|e| e.to_string())?
        );
    } else {
        for board in &boards {
            println!("{}", board.board);
        }
    }
    Ok(())
}

fn main() {
    let cli = Cli::parse();

    let result: Result<(), String> = match cli.command {
        Command::Boards { json } => boards(json),
        Command::Bundle {
            board,
            payload,
            out,
            revision,
            seabios,
        } => fbuild::bundle::export(
            &out,
            &revision,
            board.as_deref(),
            payload,
            seabios.as_deref(),
        ),
        Command::ComposeBundle {
            bundle,
            out,
            donor,
            inputs,
            signing_key,
        } => fbuild::bundle::compose(
            &bundle,
            &out,
            donor.as_deref(),
            &inputs,
            signing_key.as_deref(),
        ),
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
        Command::TestLibrary { board } => test_libraries(board.as_deref()),
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
        Command::Options { elfs } => (|| {
            use fstart_image_build::options::{from_elf, schema};
            let stages = elfs
                .iter()
                .map(|elf| {
                    let data = std::fs::read(elf).map_err(|e| format!("{elf}: {e}"))?;
                    from_elf(&data).map_err(|e| format!("{elf}: {e}"))
                })
                .collect::<Result<Vec<_>, String>>()?;
            let options = schema(stages)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&options).map_err(|e| e.to_string())?
            );
            Ok(())
        })(),
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
            stage,
        } => ide(&board, options.selection(payload), release, stage),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_tests_do_not_accept_firmware_selection_options() {
        assert!(Cli::try_parse_from(["fbuild", "test-library", "--board", "lenovo-x61"]).is_ok());
        assert!(Cli::try_parse_from(["fbuild", "test-library"]).is_ok());
        assert!(Cli::try_parse_from(["fbuild", "test-library", "--payload", "uefi"]).is_err());
    }
}
