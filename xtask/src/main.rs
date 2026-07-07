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
use xtask::{assemble, board_manifest, build_board, inspect};

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

fn dispatch_board_host(board: &str, args: &[String]) -> Result<(), String> {
    let workspace_root = build_board::workspace_root_pub()?;
    let manifest = board_manifest::find(&workspace_root, board)?;
    let status = std::process::Command::new("cargo")
        .current_dir(&workspace_root)
        .arg("run")
        .arg("--quiet")
        .arg("--manifest-path")
        .arg(manifest.dir.join("Cargo.toml"))
        .arg("--bin")
        .arg("fstart-host")
        .arg("--features")
        .arg("host")
        .arg("--")
        .args(args)
        .env("FSTART_WORKSPACE_ROOT", &workspace_root)
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

fn board_tool_assemble_args(
    subcommand: &str,
    release: bool,
    kernel: Option<String>,
    firmware: Option<String>,
) -> Vec<String> {
    let mut args = vec![subcommand.to_string()];
    if release {
        args.push("--release".to_string());
    }
    if let Some(kernel) = kernel {
        args.push("--kernel".to_string());
        args.push(kernel);
    }
    if let Some(firmware) = firmware {
        args.push("--firmware".to_string());
        args.push(firmware);
    }
    args
}

fn board_tool_run_args(
    subcommand: &str,
    release: bool,
    kernel: Option<String>,
    firmware: Option<String>,
    disk: Option<String>,
    memory: Option<String>,
) -> Vec<String> {
    let mut args = board_tool_assemble_args(subcommand, release, kernel, firmware);
    if let Some(disk) = disk {
        args.push("--disk".to_string());
        args.push(disk);
    }
    if let Some(memory) = memory {
        args.push("--memory".to_string());
        args.push(memory);
    }
    args
}

fn main() {
    let cli = Cli::parse();

    let result: Result<(), String> = match cli.command {
        Command::Build { board, release } => dispatch_board_host(
            &board,
            vec!["build".into()]
                .into_iter()
                .chain(release.then_some("--release".into()))
                .collect::<Vec<_>>()
                .as_slice(),
        ),
        Command::Run {
            board,
            release,
            kernel,
            firmware,
            disk,
            memory,
        } => dispatch_board_host(
            &board,
            &board_tool_run_args("run", release, kernel, firmware, disk, memory),
        ),
        Command::Test { board } => dispatch_board_host(&board, &["test".into()]),
        Command::Assemble {
            board,
            release,
            kernel,
            firmware,
        } => dispatch_board_host(
            &board,
            &board_tool_assemble_args("assemble", release, kernel, firmware),
        ),
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
