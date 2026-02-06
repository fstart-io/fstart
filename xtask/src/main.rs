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

fn main() {
    let cli = Cli::parse();

    let result: Result<(), String> = match cli.command {
        Command::Build { board, release } => build_board::build(&board, release).map(|_| ()),
        Command::Run { board, release } => build_board::build(&board, release)
            .and_then(|res| qemu::run(&board, &res.primary_binary().path)),
        Command::Test { board } => build_board::build(&board, true)
            .and_then(|res| qemu::run(&board, &res.primary_binary().path)),
        Command::Assemble { board } => assemble::assemble(&board).map(|_| ()),
    };

    if let Err(e) = result {
        eprintln!("error: {e}");
        process::exit(1);
    }
}
