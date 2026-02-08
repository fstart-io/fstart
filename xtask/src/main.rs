//! xtask — fstart firmware build orchestrator.
//!
//! Usage:
//!   cargo xtask build --board qemu-riscv64
//!   cargo xtask build --board qemu-riscv64 --release
//!   cargo xtask run --board qemu-riscv64
//!   cargo xtask assemble --board qemu-riscv64
//!   cargo xtask test --board qemu-riscv64

use clap::{Parser, Subcommand};
use std::process;

pub mod assemble;
pub mod build_board;
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
    },
}

/// Build and run a board in QEMU.
///
/// For monolithic boards, builds the single stage and boots it directly.
/// For multi-stage boards, assembles the full FFS image (with signing
/// and anchor patching) and boots that instead.
fn run_board(board_name: &str, release: bool) -> Result<(), String> {
    // Check if this is a multi-stage board
    let workspace_root = build_board::workspace_root_pub()?;
    let board_ron = workspace_root
        .join("boards")
        .join(board_name)
        .join("board.ron");
    let config = fstart_codegen::ron_loader::load_board_config(&board_ron)?;

    let is_multi_stage = matches!(config.stages, fstart_types::StageLayout::MultiStage(_));

    if is_multi_stage {
        // Multi-stage: assemble the FFS image and boot it
        let image_path = assemble::assemble_release(board_name, release)?;
        qemu::run(board_name, &image_path)
    } else {
        // Monolithic: build and boot the single binary directly
        let res = build_board::build(board_name, release)?;
        qemu::run(board_name, &res.primary_binary().path)
    }
}

fn main() {
    let cli = Cli::parse();

    let result: Result<(), String> = match cli.command {
        Command::Build { board, release } => build_board::build(&board, release).map(|_| ()),
        Command::Run { board, release } => run_board(&board, release),
        Command::Test { board } => run_board(&board, true),
        Command::Assemble { board } => assemble::assemble(&board).map(|_| ()),
    };

    if let Err(e) = result {
        eprintln!("error: {e}");
        process::exit(1);
    }
}
