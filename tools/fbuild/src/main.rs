use clap::{Parser, Subcommand};
use fbuild::{assemble, board_manifest, build_board, payload::PayloadChoice};
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
        #[arg(short, long)]
        board: String,
        #[arg(short, long, default_value_t = false)]
        release: bool,
        #[arg(long, value_enum)]
        payload: Option<PayloadChoice>,
    },
    Run {
        #[arg(short, long)]
        board: String,
        #[arg(short, long, default_value_t = false)]
        release: bool,
        #[arg(long, value_enum)]
        payload: Option<PayloadChoice>,
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
    },
    Test {
        #[arg(short, long)]
        board: String,
    },
    Assemble {
        #[arg(short, long)]
        board: String,
        #[arg(short, long, default_value_t = false)]
        release: bool,
        #[arg(long, value_enum)]
        payload: Option<PayloadChoice>,
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
    Flash {
        #[arg(short, long)]
        board: String,
        #[arg(short, long, default_value_t = false)]
        release: bool,
        #[arg(long, default_value_t = false)]
        probe_run: bool,
        #[arg(long)]
        chip: Option<String>,
        #[arg(long)]
        probe: Option<String>,
    },
}

fn flash_board(
    board_name: &str,
    release: bool,
    probe_run: bool,
    chip: Option<&str>,
    probe_selector: Option<&str>,
) -> Result<(), String> {
    let chip_name = chip.unwrap_or_else(|| {
        if board_name.contains("sifive-unmatched") || board_name.contains("fu740") {
            "FU740-C000"
        } else {
            "auto"
        }
    });

    let probe_rs = find_probe_rs().map_err(|e| format!("probe-rs not found: {e}"))?;
    eprintln!("[fstart] using probe-rs: {}", probe_rs.display());
    eprintln!("[fstart] chip: {chip_name}");

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

fn find_probe_rs() -> Result<std::path::PathBuf, String> {
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

    which_in_path("probe-rs").ok_or_else(|| "not in PATH or ~/src/probe-rs/target".to_string())
}

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
    let selected_workspace =
        build_board::prepare_selected_board_workspace(&workspace_root, &manifest)?;
    let status = std::process::Command::new("cargo")
        .current_dir(&workspace_root)
        .arg("run")
        .arg("--quiet")
        .arg("--manifest-path")
        .arg(selected_workspace.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(workspace_root.join("target"))
        .arg("--package")
        .arg(&manifest.package)
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

fn board_tool_build_args(
    subcommand: &str,
    release: bool,
    payload: Option<PayloadChoice>,
) -> Vec<String> {
    let mut args = vec![subcommand.to_string()];
    if release {
        args.push("--release".to_string());
    }
    if let Some(payload) = payload {
        args.push("--payload".to_string());
        args.push(payload.as_str().to_string());
    }
    args
}

fn board_tool_assemble_args(
    subcommand: &str,
    release: bool,
    payload: Option<PayloadChoice>,
    kernel: Option<String>,
    firmware: Option<String>,
    fit: Option<String>,
) -> Vec<String> {
    let mut args = board_tool_build_args(subcommand, release, payload);
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

fn board_tool_run_args(
    subcommand: &str,
    release: bool,
    payload: Option<PayloadChoice>,
    kernel: Option<String>,
    firmware: Option<String>,
    fit: Option<String>,
    disk: Option<String>,
    memory: Option<String>,
) -> Vec<String> {
    let mut args = board_tool_assemble_args(subcommand, release, payload, kernel, firmware, fit);
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
        Command::Build {
            board,
            release,
            payload,
        } => dispatch_board_host(&board, &board_tool_build_args("build", release, payload)),
        Command::Run {
            board,
            release,
            payload,
            kernel,
            firmware,
            fit,
            disk,
            memory,
        } => dispatch_board_host(
            &board,
            &board_tool_run_args("run", release, payload, kernel, firmware, fit, disk, memory),
        ),
        Command::Test { board } => dispatch_board_host(&board, &["test".into()]),
        Command::Assemble {
            board,
            release,
            payload,
            kernel,
            firmware,
            fit,
        } => dispatch_board_host(
            &board,
            &board_tool_assemble_args("assemble", release, payload, kernel, firmware, fit),
        ),
        Command::Inspect { image } => fstart_image_build::inspect::inspect(&image),
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
