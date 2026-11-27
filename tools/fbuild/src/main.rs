use clap::{Parser, Subcommand};
use fbuild::{
    board_manifest, build_board,
    payload::{BuildArgs, PayloadChoice, PlanArgs},
};
use fstart_image_build::plan::BuildSelection;
use std::process;

mod verification;

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
    /// Export what a build compiled, with its plan, as bundles an image is
    /// assembled from without a compiler (`compose-bundle`, the browser
    /// assembler), and a catalog.json. Without --board/--payload, every
    /// board and variant with every payload its platform takes.
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
    /// Assemble a bundle without Cargo or a linker.
    ComposeBundle {
        #[arg(long)]
        bundle: std::path::PathBuf,
        #[arg(long)]
        out: std::path::PathBuf,
        /// The vendor image an AMD build takes its PSP firmware from.
        #[arg(long)]
        donor: Option<std::path::PathBuf>,
        /// Named external input, e.g. kernel=bzImage.
        #[arg(long = "input")]
        inputs: Vec<String>,
        #[arg(long)]
        signing_key: Option<std::path::PathBuf>,
        /// Full raw Intel SPI backup. Preserve its non-BIOS bytes in an additional full ROM.
        #[arg(long, conflicts_with = "donor")]
        intel_flash: Option<std::path::PathBuf>,
        /// A firmware option the image's settings store starts with, as
        /// KEY=VALUE (`fbuild assemble --setting`).
        #[arg(long = "setting", value_parser = fbuild::payload::parse_setting)]
        settings: Vec<(String, String)>,
        #[command(flatten)]
        secure_boot: fbuild::payload::SecureBootArgs,
    },
    /// Print the CI firmware job matrix.
    Shards,
    /// Run CI's extra Cargo checks and architecture guard (see tools/fbuild/src/checks.rs).
    Checks {
        /// List the guard and Cargo commands without running them.
        #[arg(long)]
        list: bool,
    },
    /// Run Kani formal verification proofs across a parallel LPT work queue.
    Kani {
        /// Maximum number of concurrent Kani workers (defaults to CPU count).
        #[arg(short = 'j', long, env = "KANI_JOBS")]
        jobs: Option<usize>,
        /// List the scheduled harnesses without running them.
        #[arg(long)]
        list: bool,
        /// Optional substring filters on package or harness name.
        filters: Vec<String>,
    },
    /// Build boards in release and run their library tests, one after the
    /// other. A failing board never stops the ones after it.
    BuildBoards {
        /// Write each board's stage sizes to DIR/stage-sizes-BOARD.jsonl.
        #[arg(long, value_name = "DIR")]
        size_dir: Option<std::path::PathBuf>,
        #[arg(required = true)]
        boards: Vec<String>,
    },
    /// Generate and validate baseline ACPI tables with ACPICA; every board and
    /// variant without BOARDS (non-ACPI platforms are skipped).
    CheckAcpi { boards: Vec<String> },
    /// Boot the boards' QEMU boot tests, which their plans' run recipes
    /// declare; every board's without BOARDS.
    BootTest {
        boards: Vec<String>,
        /// Run only the tests of this name; repeatable.
        #[arg(long = "test", value_name = "NAME")]
        tests: Vec<String>,
        /// The boot assets (`nix build .#boot-assets`). Without them, the
        /// tests that need them are skipped.
        #[arg(long, value_name = "DIR")]
        assets: Option<std::path::PathBuf>,
        /// Logs of every boot. Default: target/boot-tests.
        #[arg(long, value_name = "DIR")]
        logs: Option<std::path::PathBuf>,
        /// Print the selected tests, one "BOARD TEST" line each, instead.
        #[arg(long)]
        list: bool,
        /// Report the declared tests as not run for REASON, without building or booting them.
        #[arg(long, value_name = "REASON", conflicts_with = "list")]
        not_run: Option<String>,
    },
    /// Merge and compare CI stage-size artifacts.
    Sizes {
        #[command(subcommand)]
        command: fbuild::size_report::Command,
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
        options: PlanArgs,
    },
    /// Boot the board's image under QEMU, as its plan's run recipe
    /// describes.
    Qemu {
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
        /// Complete secure pflash image for the run recipe input of that
        /// name (QEMU sbsa-ref); the same as `--input secure-firmware=PATH`.
        #[arg(long)]
        secure_firmware: Option<String>,
        /// Attach a fresh software TPM (swtpm): `1.2`, or `2.0` with
        /// `:BANK,...` for other PCR banks than SHA-1 and SHA-256.
        #[arg(long, value_name = "FAMILY[:BANKS]", value_parser = fbuild::swtpm::parse)]
        tpm: Option<fstart_image_build::boot_test::Tpm>,
    },
    /// [Deprecated] Boot the board's release image under QEMU (use `fbuild qemu --release`
    /// for interactive boots or `fbuild boot-test` for automated tests).
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
    /// Extract a board's vendor firmware blobs (AMD PSP firmware, APCBs,
    /// microcode, video BIOS) from a vendor flash image, or a vendor update
    /// ZIP holding one, into a blob directory for `--blobs`.
    Blobs {
        #[arg(short, long)]
        board: String,
        /// The vendor flash image, or the vendor's update ZIP.
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
        options: PlanArgs,
    },
    /// Print the options a build offers, as JSON: each option's key, form,
    /// label, help, legal values and the board's value, and where the
    /// board keeps stored values. The board's option sets are the only
    /// description; its platform plan runs them on the host.
    Options {
        board: String,
        #[arg(long, value_enum)]
        payload: Option<PayloadChoice>,
        #[command(flatten)]
        options: PlanArgs,
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
        options: PlanArgs,
        #[arg(long)]
        release: bool,
    },
    /// Generate an opt-in editor workspace for a resolved firmware selection.
    Ide {
        board: String,
        #[arg(long, value_enum)]
        payload: Option<PayloadChoice>,
        #[command(flatten)]
        options: PlanArgs,
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
        /// probe-rs debug protocol.
        #[arg(long, default_value = "jtag")]
        protocol: String,
        #[arg(long)]
        base_address: Option<String>,
    },
}

/// Resolve the board package the requested build selects.
fn manifest_for(board: &str) -> Result<board_manifest::BoardManifest, String> {
    let root = build_board::workspace_root()?;
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
    let root = build_board::workspace_root()?;
    let manifest = board_manifest::find(&root, board)?;
    let resolved = fbuild::plan_executor::Resolved::load(&root, &manifest, selection)?;
    println!("{}", resolved.explain_json()?);
    Ok(())
}

/// The options a build offers, as JSON (`fbuild options`).
fn print_options(board: &str, selection: BuildSelection) -> Result<(), String> {
    let root = build_board::workspace_root()?;
    let manifest = board_manifest::find(&root, board)?;
    let resolved = fbuild::plan_executor::Resolved::load(&root, &manifest, selection)?;
    let options = serde_json::to_string_pretty(resolved.options()).map_err(|e| e.to_string())?;
    println!("{options}");
    Ok(())
}

fn compile_named(
    board: &str,
    selection: BuildSelection,
    release: bool,
    name: &str,
    checking: bool,
) -> Result<(), String> {
    let root = build_board::workspace_root()?;
    let manifest = board_manifest::find(&root, board)?;
    let resolved = fbuild::plan_executor::Resolved::load(&root, &manifest, selection)?;
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
    let root = build_board::workspace_root()?;
    let manifest = board_manifest::find(&root, board)?;
    let resolved = fbuild::plan_executor::Resolved::load(&root, &manifest, selection)?;
    resolved.check(&root, &manifest, release)
}

fn ide(
    board: &str,
    selection: BuildSelection,
    release: bool,
    stage: Option<String>,
) -> Result<(), String> {
    let root = build_board::workspace_root()?;
    let manifest = board_manifest::find(&root, board)?;
    let resolved = fbuild::plan_executor::Resolved::load(&root, &manifest, selection)?;
    let workspace =
        fbuild::ide::generate_image(&root, &manifest, &resolved, stage.as_deref(), release)?;
    println!("{}", workspace.display());
    Ok(())
}

fn test_libraries(board: Option<&str>) -> Result<(), String> {
    if let Some(board) = board {
        return fbuild::board_tool::test_library(&manifest_for(board)?);
    }
    let root = build_board::workspace_root()?;
    let boards = board_manifest::discover(&root)?;
    for board in &boards {
        fbuild::board_tool::test_library(board)?;
        for variant in board.variants.keys() {
            fbuild::board_tool::test_library(&board_manifest::find_in(&boards, variant)?)?;
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
    arch: Option<&'a str>,
    build_profile: Option<&'a board_manifest::ProfileRef>,
    variants: Vec<&'a str>,
}

fn boards(json: bool) -> Result<(), String> {
    let root = build_board::workspace_root()?;
    let boards = board_manifest::discover(&root)?;
    if json {
        let entries: Vec<_> = boards
            .iter()
            .map(|board| BoardEntry {
                board: &board.board,
                package: &board.package,
                dir: board.rel_dir.display().to_string(),
                arch: board.arch.as_deref(),
                build_profile: board.build_profile.as_ref(),
                variants: board.variants.keys().map(String::as_str).collect(),
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
            intel_flash,
            inputs,
            signing_key,
            settings,
            secure_boot,
        } => fbuild::payload::store_settings(&settings, &secure_boot).and_then(|settings| {
            fbuild::bundle::compose(
                &bundle,
                &out,
                donor.as_deref(),
                intel_flash.as_deref(),
                &inputs,
                signing_key.as_deref(),
                &settings,
            )
        }),
        Command::Shards => fbuild::shards::run(),
        Command::Checks { list } => fbuild::checks::run(list),
        Command::Kani {
            jobs,
            list,
            filters,
        } => fbuild::kani::run(jobs, list, &filters),
        Command::BuildBoards { size_dir, boards } => verification::build_boards(&boards, size_dir),
        Command::CheckAcpi { boards } => fbuild::acpi_check::check(&boards),
        Command::BootTest {
            boards,
            tests,
            assets,
            logs,
            list,
            not_run,
        } => verification::boot_test(&boards, &tests, assets, logs, list, not_run.as_deref()),
        Command::Sizes { command } => command.run(),
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
        Command::Qemu {
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
            tpm,
        } => manifest_for(&board).and_then(|manifest| {
            fbuild::board_tool::qemu(
                &manifest,
                release,
                payload,
                &fbuild::board_tool::QemuArgs {
                    kernel: kernel.as_deref(),
                    firmware: firmware.as_deref(),
                    fit: fit.as_deref(),
                    disk: disk.as_deref(),
                    memory: memory.as_deref(),
                    secure_firmware: secure_firmware.as_deref(),
                    tpm,
                },
                &options,
            )
        }),
        Command::Test { board, options } => {
            eprintln!(
                "warning: `fbuild test` is deprecated; use `fbuild qemu --release --board {board}` \
                 (or `fbuild boot-test` for automated QEMU tests)"
            );
            manifest_for(&board).and_then(|manifest| {
                fbuild::board_tool::qemu(&manifest, true, None, &Default::default(), &options)
            })
        }
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
        Command::Blobs {
            board,
            from,
            out,
            allow_missing,
        } => build_board::workspace_root().and_then(|root| {
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
        Command::Options {
            board,
            payload,
            options,
        } => print_options(&board, options.selection(payload)),
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
            protocol,
            base_address,
        } => manifest_for(&board).and_then(|manifest| {
            fbuild::board_tool::flash(
                &manifest,
                release,
                &options,
                probe_run,
                chip.as_deref(),
                probe.as_deref(),
                &protocol,
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
        assert!(Cli::try_parse_from(["fbuild", "test-library", "--board", "test-board"]).is_ok());
        assert!(Cli::try_parse_from(["fbuild", "test-library"]).is_ok());
        assert!(Cli::try_parse_from(["fbuild", "test-library", "--payload", "uefi"]).is_err());
    }

    #[test]
    fn plan_only_commands_reject_assembly_only_flags() {
        let plan_only: &[&[&str]] = &[
            &["fbuild", "build", "--board", "b"],
            &["fbuild", "check", "b"],
            &["fbuild", "explain", "b"],
            &["fbuild", "options", "b"],
            &["fbuild", "ide", "b"],
        ];
        let assembly_flags: &[&[&str]] = &[
            &["--signing-key", "key.pem"],
            &["--setting", "k=v"],
            &["--secure-boot", "on"],
            &["--payload-file", "a=b"],
            &["--input", "kernel=bzImage"],
        ];
        for base in plan_only {
            assert!(Cli::try_parse_from(*base).is_ok(), "{base:?}");
            let mut with_plan = base.to_vec();
            with_plan.extend(["--log-level", "debug", "--option", "foo=bar"]);
            assert!(Cli::try_parse_from(&with_plan).is_ok(), "{with_plan:?}");
            for flag in assembly_flags {
                let mut argv = base.to_vec();
                argv.extend(*flag);
                assert!(
                    Cli::try_parse_from(&argv).is_err(),
                    "expected rejection for {argv:?}"
                );
            }
        }
        // Assembly commands accept both plan and assembly flags.
        for base in [
            &["fbuild", "assemble", "--board", "b"][..],
            &["fbuild", "qemu", "--board", "b"],
            &["fbuild", "flash", "--board", "b"],
            &["fbuild", "test", "--board", "b"],
        ] {
            let mut argv = base.to_vec();
            argv.extend([
                "--log-level",
                "debug",
                "--signing-key",
                "key.pem",
                "--setting",
                "k=v",
                "--input",
                "kernel=bzImage",
            ]);
            assert!(Cli::try_parse_from(&argv).is_ok(), "{argv:?}");
        }
    }
}
