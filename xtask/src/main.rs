//! xtask — fstart firmware build orchestrator.
//!
//! Usage:
//!   cargo xtask build --board qemu-riscv64
//!   cargo xtask build --board qemu-riscv64 --release
//!   cargo xtask run --board qemu-riscv64
//!   cargo xtask assemble --board qemu-riscv64
//!   cargo xtask inspect --image target/ffs/qemu-riscv64.ffs
//!   cargo xtask test --board qemu-riscv64
//!   cargo xtask flash --board sifive-unmatched-hw
//!   cargo xtask flash --board sifive-unmatched-hw --probe-run

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
        /// Path to disk image (qcow2/raw) — attached as NVMe
        #[arg(short, long)]
        disk: Option<String>,
        /// Amount of RAM (e.g., "1G", "512M"). Default: QEMU default.
        #[arg(short, long)]
        memory: Option<String>,
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
    /// Flash firmware to real hardware via probe-rs JTAG
    Flash {
        /// Board name
        #[arg(short, long)]
        board: String,
        /// Build in release mode
        #[arg(short, long, default_value_t = false)]
        release: bool,
        /// Use `probe-rs run` (load to RAM + execute) instead of `probe-rs download` (flash to SPI NOR)
        #[arg(long, default_value_t = false)]
        probe_run: bool,
        /// probe-rs chip name (default: auto-detect from board config)
        #[arg(long)]
        chip: Option<String>,
        /// probe-rs probe selector (e.g., "0403:6010")
        #[arg(long)]
        probe: Option<String>,
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
    disk: Option<&str>,
    memory: Option<&str>,
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
        qemu::run(board_name, config.platform, &image_path, disk, memory)
    } else {
        // Simple monolithic: build and boot the single binary directly
        let res = build_board::build(board_name, release)?;
        qemu::run(
            board_name,
            config.platform,
            &res.primary_binary().run_path,
            disk,
            memory,
        )
    }
}

/// Build and flash firmware to real hardware via probe-rs.
///
/// Two-step process:
/// 1. Assemble FFS (stage + LZ4-compressed payloads) and flash to SPI NOR
///    via `probe-rs download --binary-format bin --base-address 0x20000000`
/// 2. Load stage ELF to L2 LIM via `probe-rs run` (JTAG RAM load + execute)
///
/// If `--probe-run` is set, skips the SPI NOR flash step and only loads
/// the stage ELF to LIM (assumes FFS was previously flashed).
fn flash_board(
    board_name: &str,
    release: bool,
    probe_run: bool,
    chip: Option<&str>,
    probe_selector: Option<&str>,
) -> Result<(), String> {
    // Determine chip name — default to FU740-C000 for sifive-unmatched boards.
    let chip_name = chip.unwrap_or_else(|| {
        if board_name.contains("sifive-unmatched") || board_name.contains("fu740") {
            "FU740-C000"
        } else {
            "auto"
        }
    });

    // Find probe-rs binary.
    let probe_rs = find_probe_rs().map_err(|e| format!("probe-rs not found: {e}"))?;
    eprintln!("[fstart] using probe-rs: {}", probe_rs.display());
    eprintln!("[fstart] chip: {chip_name}");

    // Helper to build a probe-rs command with common args.
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
        // Step 1: Assemble FFS with LZ4-compressed payloads and flash to SPI NOR.
        eprintln!("[fstart] step 1/2: assembling FFS and flashing to SPI NOR...");
        let ffs_path = assemble::assemble_with_opts(board_name, release, None, None)?;
        let ffs_size = std::fs::metadata(&ffs_path).map(|m| m.len()).unwrap_or(0);

        if ffs_size > 32 * 1024 * 1024 {
            return Err(format!(
                "FFS image ({} bytes) exceeds 32 MiB SPI NOR capacity",
                ffs_size
            ));
        }

        eprintln!(
            "[fstart] flashing FFS ({:.1} MiB) to SPI NOR at 0x20000000...",
            ffs_size as f64 / (1024.0 * 1024.0)
        );

        let mut cmd = mk_cmd("download");
        cmd.arg("--binary-format")
            .arg("bin")
            .arg("--base-address")
            .arg("0x20000000")
            .arg(&ffs_path);

        eprintln!("[fstart] running: {:?}", cmd);
        let status = cmd
            .status()
            .map_err(|e| format!("failed to run probe-rs download: {e}"))?;
        if !status.success() {
            return Err(format!("probe-rs download (SPI NOR) failed with {status}"));
        }
        eprintln!("[fstart] SPI NOR flash complete.");
    } else {
        eprintln!("[fstart] --probe-run: skipping SPI NOR flash (using existing FFS)");
    }

    // Step 2: Build stage and load to L2 LIM via probe-rs run.
    eprintln!("[fstart] step 2/2: loading stage to L2 LIM via JTAG...");
    let res = build_board::build(board_name, release)?;
    let elf_path = &res.primary_binary().path;
    eprintln!("[fstart] ELF: {}", elf_path.display());

    let mut cmd = mk_cmd("run");
    cmd.arg(elf_path);

    eprintln!("[fstart] running: {:?}", cmd);
    eprintln!("[fstart] === UART output should appear on /dev/ttyUSB1 (115200 baud) ===");
    let status = cmd
        .status()
        .map_err(|e| format!("failed to run probe-rs run: {e}"))?;
    if !status.success() {
        return Err(format!("probe-rs run failed with {status}"));
    }
    Ok(())
}

/// Find probe-rs binary: check ~/src/probe-rs/target first, then PATH.
fn find_probe_rs() -> Result<std::path::PathBuf, String> {
    // Check the local fork build first.
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

    // Fall back to PATH.
    which_in_path("probe-rs").ok_or_else(|| "not in PATH or ~/src/probe-rs/target".to_string())
}

/// Simple which(1) implementation: find an executable on PATH.
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

fn main() {
    let cli = Cli::parse();

    let result: Result<(), String> = match cli.command {
        Command::Build { board, release } => build_board::build(&board, release).map(|_| ()),
        Command::Run {
            board,
            release,
            kernel,
            firmware,
            disk,
            memory,
        } => run_board(
            &board,
            release,
            kernel.as_deref(),
            firmware.as_deref(),
            disk.as_deref(),
            memory.as_deref(),
        ),
        Command::Test { board } => run_board(&board, true, None, None, None, None),
        Command::Assemble {
            board,
            release,
            kernel,
            firmware,
        } => assemble::assemble_with_opts(&board, release, kernel.as_deref(), firmware.as_deref())
            .map(|_| ()),
        Command::Inspect { image } => inspect::inspect(&image),
        Command::Flash {
            board,
            release,
            probe_run,
            chip,
            probe,
        } => flash_board(
            &board,
            release,
            probe_run,
            chip.as_deref(),
            probe.as_deref(),
        ),
    };

    if let Err(e) = result {
        eprintln!("error: {e}");
        process::exit(1);
    }
}
