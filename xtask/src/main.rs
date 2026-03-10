//! xtask — fstart firmware build orchestrator.
//!
//! Usage:
//!   cargo xtask build --board qemu-riscv64
//!   cargo xtask build --board qemu-riscv64 --release
//!   cargo xtask run --board qemu-riscv64
//!   cargo xtask assemble --board qemu-riscv64
//!   cargo xtask inspect --image target/ffs/qemu-riscv64.ffs
//!   cargo xtask test --board qemu-riscv64

use clap::{Parser, Subcommand};
use std::process;

pub mod assemble;
pub mod build_board;
mod inspect;
mod qemu;

#[derive(Parser)]
#[command(name = "xtask", about = "fstart firmware build orchestrator")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Build firmware image for a board
    Build {
        /// Board name (directory under boards/)
        #[arg(short, long)]
        board: String,
        /// Build in release mode
        #[arg(short, long, default_value_t = false)]
        release: bool,
    },
    /// Build and run in QEMU
    Run {
        /// Board name
        #[arg(short, long)]
        board: String,
        /// Build in release mode
        #[arg(short, long, default_value_t = false)]
        release: bool,
        /// Path to kernel binary (for LinuxBoot payloads)
        #[arg(short, long)]
        kernel: Option<String>,
        /// Path to firmware binary (OpenSBI/ATF, for LinuxBoot payloads)
        #[arg(short, long)]
        firmware: Option<String>,
    },
    /// Build and run tests in QEMU
    Test {
        /// Board name
        #[arg(short, long)]
        board: String,
    },
    /// Assemble an FFS firmware image (build first, then package)
    Assemble {
        /// Board name
        #[arg(short, long)]
        board: String,
        /// Build in release mode
        #[arg(short, long, default_value_t = false)]
        release: bool,
        /// Path to kernel binary (for LinuxBoot payloads)
        #[arg(short, long)]
        kernel: Option<String>,
        /// Path to firmware binary (OpenSBI/ATF, for LinuxBoot payloads)
        #[arg(short, long)]
        firmware: Option<String>,
    },
    /// Inspect an FFS firmware image (find anchor, display filesystem)
    Inspect {
        /// Path to FFS image file
        #[arg(short, long)]
        image: String,
    },
}

/// Build and run a board in QEMU.
///
/// For monolithic boards without external payload blobs, builds the single
/// stage and boots it directly. For multi-stage boards or boards with
/// LinuxBoot payloads (which need firmware + kernel in FFS), assembles the
/// full FFS image first.
fn run_board(
    board_name: &str,
    release: bool,
    kernel: Option<&str>,
    firmware: Option<&str>,
) -> Result<(), String> {
    // Check if this board needs assembly (multi-stage or has payload blobs)
    let workspace_root = build_board::workspace_root_pub()?;
    let board_ron = workspace_root
        .join("boards")
        .join(board_name)
        .join("board.ron");
    let config = fstart_codegen::ron_loader::load_board_config(&board_ron)?;

    let is_multi_stage = matches!(config.stages, fstart_types::StageLayout::MultiStage(_));
    let has_payload_blobs = kernel.is_some()
        || firmware.is_some()
        || config.payload.as_ref().is_some_and(|p| {
            p.firmware.is_some()
                || p.kernel_file.is_some()
                || p.kind == fstart_types::PayloadKind::FitImage
        });

    if is_multi_stage || has_payload_blobs {
        // Assemble the FFS image (includes stage + firmware + kernel)
        let image_path = assemble::assemble_with_opts(board_name, release, kernel, firmware)?;
        qemu::run(board_name, config.platform, &image_path)
    } else {
        // Simple monolithic: build and boot the single binary directly
        let res = build_board::build(board_name, release)?;
        qemu::run(board_name, config.platform, &res.primary_binary().run_path)
    }
}

fn main() {
    let cli = Cli::parse();

    let result: Result<(), String> = match cli.command {
        Command::Build { board, release } => build_board::build(&board, release).map(|_| ()),
        Command::Run {
            board,
            release,
            kernel,
            firmware,
        } => run_board(&board, release, kernel.as_deref(), firmware.as_deref()),
        Command::Test { board } => run_board(&board, true, None, None),
        Command::Assemble {
            board,
            release,
            kernel,
            firmware,
        } => assemble::assemble_with_opts(&board, release, kernel.as_deref(), firmware.as_deref())
            .map(|_| ()),
        Command::Inspect { image } => inspect::inspect(&image),
    };

    if let Err(e) = result {
        eprintln!("error: {e}");
        process::exit(1);
    }
}
