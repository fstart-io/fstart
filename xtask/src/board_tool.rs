//! Board-owned host tool entry points.
//!
//! The generated host-tool crate calls this module with the selected board
//! crate's Rust metadata functions. The metadata stays in-process as typed Rust
//! values.

use clap::{Parser, Subcommand};
use fstart_board_meta::DriverBinding;
use fstart_codegen::board_loader::{
    load_parsed_board_from_rust_with_acpi, load_parsed_board_metadata_only, ParsedBoard,
};
use fstart_types::acpi::AcpiExtraDevice;
use fstart_types::{BoardConfig, BuildInfo, StageLayout};

/// Rust callbacks exported by a board crate for host tooling.
pub struct BoardCallbacks {
    pub board_config: fn() -> BoardConfig,
    pub build_info: fn() -> BuildInfo,
    pub driver_bindings: Option<fn() -> Vec<DriverBinding>>,
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

/// Run a board-owned host tool using typed Rust metadata callbacks.
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
) -> Result<(crate::board_manifest::BoardManifest, BuildInfo, ParsedBoard), String> {
    let config = (callbacks.board_config)();
    let build_info = (callbacks.build_info)();
    let acpi_only_devices = callbacks
        .acpi_only_devices
        .map_or_else(Vec::new, |load| load());
    let workspace_root = crate::build_board::workspace_root_pub()?;
    let manifest = crate::board_manifest::find(&workspace_root, config.name.as_str())?;
    let parsed = if manifest.stage_package.is_some() {
        load_parsed_board_metadata_only(config, acpi_only_devices)?
    } else {
        let load_drivers = callbacks.driver_bindings.ok_or_else(|| {
            format!(
                "legacy board '{}' must provide driver_bindings or declare stage-package",
                manifest.board
            )
        })?;
        load_parsed_board_from_rust_with_acpi(config, load_drivers(), acpi_only_devices)?
    };
    validate(&manifest, &build_info)?;
    Ok((manifest, build_info, parsed))
}

fn build(
    callbacks: BoardCallbacks,
    release: bool,
) -> Result<crate::build_board::BuildResult, String> {
    let workspace_root = crate::build_board::workspace_root_pub()?;
    let (manifest, build_info, parsed) = load(callbacks)?;
    crate::build_board::build_with_parsed(&workspace_root, &manifest, build_info, &parsed, release)
}

fn assemble(
    callbacks: BoardCallbacks,
    release: bool,
    kernel: Option<&str>,
    firmware: Option<&str>,
) -> Result<std::path::PathBuf, String> {
    let workspace_root = crate::build_board::workspace_root_pub()?;
    let (manifest, build_info, parsed) = load(callbacks)?;
    crate::assemble::assemble_with_parsed(
        &workspace_root,
        manifest,
        build_info,
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
                || p.kind == fstart_types::PayloadKind::FitImage
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
    info: &BuildInfo,
) -> Result<(), String> {
    if info.name.as_str() != manifest.board {
        return Err(format!(
            "build_info name mismatch for {}: manifest board is '{}', board returned '{}'",
            manifest.package, manifest.board, info.name
        ));
    }
    if info.board_package.as_str() != manifest.package {
        return Err(format!(
            "build_info package mismatch for {}: board returned '{}'",
            manifest.package, info.board_package
        ));
    }
    if let Some(target) = &manifest.target {
        if info.target.as_str() != target {
            return Err(format!(
                "build_info target mismatch for {}: manifest target is '{}', board returned '{}'",
                manifest.package, target, info.target
            ));
        }
    }
    Ok(())
}
