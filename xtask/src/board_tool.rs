//! Board-owned host tool entry points.
//!
//! A board-owned `fstart-host` binary calls this module with the selected
//! board crate's Rust config function. Host build facts come from the board
//! `Cargo.toml` `[package.metadata.fstart]` table.

use clap::{Parser, Subcommand};
use fstart_core::acpi::AcpiExtraDevice;
use fstart_core::{BoardConfig, StageLayout};

use crate::build_plan::ParsedBoard;

/// Rust callbacks exported by a board crate for host tooling.
pub struct BoardCallbacks {
    pub board_config: fn() -> BoardConfig,
    pub acpi_only_devices: Option<fn() -> Vec<AcpiExtraDevice>>,
}

#[derive(Parser)]
#[command(name = "fstart-board-tool", about = "board-owned fstart build tool")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Build {
        #[arg(short, long, default_value_t = false)]
        release: bool,
    },
    Run {
        #[arg(short, long, default_value_t = false)]
        release: bool,
        #[arg(short, long)]
        kernel: Option<String>,
        #[arg(short, long)]
        firmware: Option<String>,
        #[arg(short, long)]
        disk: Option<String>,
        #[arg(short, long)]
        memory: Option<String>,
    },
    Test,
    Assemble {
        #[arg(short, long, default_value_t = false)]
        release: bool,
        #[arg(short, long)]
        kernel: Option<String>,
        #[arg(short, long)]
        firmware: Option<String>,
    },
}

/// Declare a board-owned host tool entry point.
#[macro_export]
macro_rules! board_host_tool {
    ($board_config:path) => {
        fn main() {
            xtask::board_tool::main(xtask::board_tool::BoardCallbacks {
                board_config: $board_config,
                acpi_only_devices: None,
            });
        }
    };
    ($board_config:path, acpi_only_devices = $acpi_only_devices:path) => {
        fn main() {
            xtask::board_tool::main(xtask::board_tool::BoardCallbacks {
                board_config: $board_config,
                acpi_only_devices: Some($acpi_only_devices),
            });
        }
    };
}

/// Run a board-owned host tool using typed Rust config and Cargo metadata.
pub fn main(callbacks: BoardCallbacks) {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Build { release } => build(callbacks, release).map(|_| ()),
        Command::Run {
            release,
            kernel,
            firmware,
            disk,
            memory,
        } => run(
            callbacks,
            release,
            kernel.as_deref(),
            firmware.as_deref(),
            disk.as_deref(),
            memory.as_deref(),
        ),
        Command::Test => run(callbacks, true, None, None, None, None),
        Command::Assemble {
            release,
            kernel,
            firmware,
        } => assemble(callbacks, release, kernel.as_deref(), firmware.as_deref()).map(|_| ()),
    };

    if let Err(err) = result {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn load(
    callbacks: BoardCallbacks,
) -> Result<(crate::board_manifest::BoardManifest, ParsedBoard), String> {
    let mut config = (callbacks.board_config)();
    config
        .memory
        .normalize_derived_flash()
        .map_err(|err| err.to_string())?;
    let acpi_only_devices = callbacks
        .acpi_only_devices
        .map_or_else(Vec::new, |load| load());
    let workspace_root = crate::build_board::workspace_root_pub()?;
    let manifest = crate::board_manifest::find(&workspace_root, config.name.as_str())?;
    validate(&manifest, &config)?;
    Ok((
        manifest,
        ParsedBoard {
            config,
            acpi_only_devices,
        },
    ))
}

fn build(
    callbacks: BoardCallbacks,
    release: bool,
) -> Result<crate::build_board::BuildResult, String> {
    let workspace_root = crate::build_board::workspace_root_pub()?;
    let (manifest, parsed) = load(callbacks)?;
    crate::build_board::build_with_parsed(&workspace_root, &manifest, &parsed, release)
}

fn assemble(
    callbacks: BoardCallbacks,
    release: bool,
    kernel: Option<&str>,
    firmware: Option<&str>,
) -> Result<std::path::PathBuf, String> {
    let workspace_root = crate::build_board::workspace_root_pub()?;
    let (manifest, parsed) = load(callbacks)?;
    crate::assemble::assemble_with_parsed(
        &workspace_root,
        manifest,
        parsed,
        release,
        kernel,
        firmware,
    )
}

fn run(
    callbacks: BoardCallbacks,
    release: bool,
    kernel: Option<&str>,
    firmware: Option<&str>,
    disk: Option<&str>,
    memory: Option<&str>,
) -> Result<(), String> {
    let config = (callbacks.board_config)();
    let is_multi_stage = matches!(config.stages, StageLayout::MultiStage(_));
    let has_payload_blobs = kernel.is_some()
        || firmware.is_some()
        || config.payload.as_ref().is_some_and(|p| {
            p.firmware.is_some()
                || p.kernel_file.is_some()
                || p.kind == fstart_core::PayloadKind::FitImage
        });

    if is_multi_stage || has_payload_blobs {
        let image_path = assemble(callbacks, release, kernel, firmware)?;
        crate::qemu::run(
            config.name.as_str(),
            config.platform,
            &image_path,
            disk,
            memory,
        )
    } else {
        let res = build(callbacks, release)?;
        crate::qemu::run(
            config.name.as_str(),
            config.platform,
            &res.primary_binary().run_path,
            disk,
            memory,
        )
    }
}

fn validate(
    manifest: &crate::board_manifest::BoardManifest,
    config: &BoardConfig,
) -> Result<(), String> {
    if config.name.as_str() != manifest.board {
        return Err(format!(
            "board config name mismatch for {}: manifest board is '{}', board returned '{}'",
            manifest.package, manifest.board, config.name
        ));
    }
    if let Some(platform) = &manifest.platform {
        if config.platform.as_str() != platform {
            return Err(format!(
                "board platform mismatch for {}: manifest platform is '{}', board returned '{}'",
                manifest.package, platform, config.platform
            ));
        }
    }
    if let Some(target) = &manifest.target {
        if config.platform.target_triple() != target {
            return Err(format!(
                "board target mismatch for {}: manifest target is '{}', platform implies '{}'",
                manifest.package,
                target,
                config.platform.target_triple()
            ));
        }
    }
    Ok(())
}
