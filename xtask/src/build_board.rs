//! Board build orchestration.
//!
//! 1. Parse board.ron
//! 2. Determine target triple, cargo features, and environment
//! 3. Invoke cargo build on fstart-stage (once for monolithic, per-stage for multi-stage)
//! 4. Return the path(s) to the built binary(ies)

use fstart_codegen::ron_loader;
use fstart_types::StageLayout;
use std::path::PathBuf;
use std::process::Command;

/// Result of building a board — one or more stage binaries.
pub struct BuildResult {
    /// Built stage binaries, in order. For monolithic boards this has one entry
    /// with name "stage". For multi-stage boards it has one entry per stage.
    pub stages: Vec<StageBinary>,
}

/// A built stage binary.
pub struct StageBinary {
    /// Stage name (e.g., "bootblock", "main", or "stage" for monolithic).
    pub name: String,
    /// Path to the ELF binary on disk (used by assembler for objcopy).
    pub path: PathBuf,
    /// Path to run in QEMU (flat binary for AArch64, same as `path` otherwise).
    pub run_path: PathBuf,
    /// Load address from the board config.
    pub load_addr: u64,
}

impl BuildResult {
    /// Get the first (or only) binary — used for QEMU boot.
    pub fn primary_binary(&self) -> &StageBinary {
        &self.stages[0]
    }
}

/// Build firmware for the given board. Returns all stage binaries.
pub fn build(board_name: &str, release: bool) -> Result<BuildResult, String> {
    let workspace_root = workspace_root()?;
    let board_dir = workspace_root.join("boards").join(board_name);
    let board_ron = board_dir.join("board.ron");

    if !board_ron.exists() {
        return Err(format!("board config not found: {}", board_ron.display()));
    }

    eprintln!("[fstart] loading board config: {}", board_ron.display());
    let config = ron_loader::load_board_config(&board_ron)?;

    eprintln!("[fstart] board: {}", config.name);
    eprintln!("[fstart] platform: {}", config.platform);
    eprintln!("[fstart] mode: {:?}", config.mode);

    // Determine target triple
    let target = match config.platform.as_str() {
        "riscv64" => "riscv64gc-unknown-none-elf",
        "aarch64" => "aarch64-unknown-none",
        "armv7" => "armv7a-none-eabi",
        other => return Err(format!("unsupported platform: {other}")),
    };

    // Collect features: platform + drivers + FFS/crypto (if any stage uses FFS caps)
    let mut features = Vec::new();
    features.push(config.platform.to_string());
    for device in &config.devices {
        features.push(device.driver.to_string());
    }

    // Check if any stage uses FFS capabilities (SigVerify, StageLoad, PayloadLoad)
    let uses_ffs = match &config.stages {
        StageLayout::Monolithic(mono) => mono.capabilities.iter().any(|c| {
            matches!(
                c,
                fstart_types::Capability::SigVerify
                    | fstart_types::Capability::StageLoad { .. }
                    | fstart_types::Capability::PayloadLoad
            )
        }),
        StageLayout::MultiStage(stages) => stages.iter().any(|s| {
            s.capabilities.iter().any(|c| {
                matches!(
                    c,
                    fstart_types::Capability::SigVerify
                        | fstart_types::Capability::StageLoad { .. }
                        | fstart_types::Capability::PayloadLoad
                )
            })
        }),
    };

    if uses_ffs {
        features.push("ffs".to_string());
        // LZ4 decompression support — always enabled when FFS is active
        // so the runtime can handle compressed stage/payload segments.
        features.push("lz4".to_string());
        // Enable crypto features based on board security config
        match config.security.signing_algorithm {
            fstart_types::SignatureAlgorithm::Ed25519 => features.push("ed25519".to_string()),
            fstart_types::SignatureAlgorithm::EcdsaP256 => {} // future: add ecdsa feature
        }
        for digest in &config.security.required_digests {
            match digest {
                fstart_types::DigestAlgorithm::Sha256 => features.push("sha2-digest".to_string()),
                fstart_types::DigestAlgorithm::Sha3_256 => features.push("sha3-digest".to_string()),
            }
        }
    }

    // Check if any stage uses FDT capabilities (FdtPrepare)
    let uses_fdt = match &config.stages {
        StageLayout::Monolithic(mono) => mono
            .capabilities
            .iter()
            .any(|c| matches!(c, fstart_types::Capability::FdtPrepare)),
        StageLayout::MultiStage(stages) => stages.iter().any(|s| {
            s.capabilities
                .iter()
                .any(|c| matches!(c, fstart_types::Capability::FdtPrepare))
        }),
    };

    if uses_fdt {
        features.push("fdt".to_string());
    }

    // Check if the board uses a FIT image with runtime parsing
    let uses_fit_runtime = config.payload.as_ref().is_some_and(|p| {
        p.kind == fstart_types::PayloadKind::FitImage
            && p.fit_parse.unwrap_or(fstart_types::FitParseMode::Buildtime)
                == fstart_types::FitParseMode::Runtime
    });
    if uses_fit_runtime {
        features.push("fit".to_string());
    }

    let features_str = features.join(",");

    eprintln!("[fstart] target: {target}");
    eprintln!("[fstart] features: {features_str}");

    // Both AArch64 and RISC-V need flat binaries for QEMU: AArch64 uses
    // -bios which expects raw binary, RISC-V uses pflash which also needs
    // raw binary data.
    let needs_flat_binary = matches!(config.platform.as_str(), "aarch64" | "riscv64" | "armv7");

    // Determine build-std components: always need core, add alloc when FDT
    // feature is enabled (dtoolkit write API + bump allocator need alloc).
    let build_std = if uses_fdt { "core,alloc" } else { "core" };

    match &config.stages {
        StageLayout::Monolithic(mono) => {
            // Single build, no FSTART_STAGE_NAME needed
            let (elf_path, run_path) = build_one_stage(
                &workspace_root,
                &board_ron,
                None,
                target,
                &features_str,
                release,
                needs_flat_binary,
                build_std,
            )?;
            Ok(BuildResult {
                stages: vec![StageBinary {
                    name: "stage".to_string(),
                    path: elf_path,
                    run_path,
                    load_addr: mono.load_addr,
                }],
            })
        }
        StageLayout::MultiStage(stages) => {
            let mut result = Vec::new();
            for stage in stages {
                let stage_name = stage.name.to_string();
                eprintln!("[fstart] building stage: {stage_name}");
                let (elf_path, run_path) = build_one_stage(
                    &workspace_root,
                    &board_ron,
                    Some(&stage_name),
                    target,
                    &features_str,
                    release,
                    needs_flat_binary,
                    build_std,
                )?;
                result.push(StageBinary {
                    name: stage_name,
                    path: elf_path,
                    run_path,
                    load_addr: stage.load_addr,
                });
            }
            Ok(BuildResult { stages: result })
        }
    }
}

/// Build a single fstart-stage binary.
///
/// `stage_name` is `None` for monolithic, `Some("bootblock")` etc. for multi-stage.
#[allow(clippy::too_many_arguments)]
/// Returns (elf_path, run_path). For AArch64 and RISC-V these differ
/// (ELF vs flat binary for QEMU); for other platforms they are the same.
fn build_one_stage(
    workspace_root: &std::path::Path,
    board_ron: &std::path::Path,
    stage_name: Option<&str>,
    target: &str,
    features: &str,
    release: bool,
    needs_flat_binary: bool,
    build_std: &str,
) -> Result<(PathBuf, PathBuf), String> {
    let mut cmd = Command::new("cargo");
    cmd.arg("build")
        .arg("--package")
        .arg("fstart-stage")
        .arg("--target")
        .arg(target)
        .arg("--features")
        .arg(features)
        .arg("-Z")
        .arg(format!("build-std={build_std}"));

    if release {
        cmd.arg("--release");
    }

    // Disable UB precondition checks on volatile ops (nightly core library
    // adds alignment/null checks on read_volatile/write_volatile that are
    // incompatible with MMIO register access in firmware debug builds).
    cmd.env("RUSTFLAGS", "-Zub-checks=no");

    // Pass board RON path to build.rs
    cmd.env("FSTART_BOARD_RON", board_ron.to_str().unwrap());
    if let Some(name) = stage_name {
        cmd.env("FSTART_STAGE_NAME", name);
    }

    eprintln!("[fstart] building fstart-stage...");
    let status = cmd
        .status()
        .map_err(|e| format!("failed to run cargo: {e}"))?;
    if !status.success() {
        return Err(format!(
            "build failed{}",
            stage_name
                .map(|n| format!(" for stage '{n}'"))
                .unwrap_or_default()
        ));
    }

    // Determine output binary path
    let profile = if release { "release" } else { "debug" };
    let elf_path = workspace_root
        .join("target")
        .join(target)
        .join(profile)
        .join("fstart-stage");

    // For multi-stage: copy the binary to a stage-specific name so subsequent
    // builds don't overwrite it (cargo always outputs to "fstart-stage").
    let final_elf = if let Some(name) = stage_name {
        let dest = elf_path.with_file_name(format!("fstart-{name}"));
        std::fs::copy(&elf_path, &dest).map_err(|e| format!("failed to copy stage binary: {e}"))?;
        dest
    } else {
        elf_path.clone()
    };

    // Produce a flat binary for QEMU. AArch64 uses -bios which needs a raw
    // binary; RISC-V uses pflash which also needs raw binary data.
    //
    // Both platforms use XIP (code in ROM, data in RAM). The .data
    // section's LMA is in ROM (via `AT > ROM` in the linker script) so it
    // is contiguous with .text/.rodata and must NOT be removed — the _start
    // assembly copies those initializers to RAM. Only .bss is removed: it
    // is NOLOAD and its VMA is in RAM, which would cause objcopy to span
    // the ROM→RAM gap (producing a multi-GiB file of mostly zeros). The
    // entry code clears BSS at runtime.
    let run_path = if needs_flat_binary {
        let bin_path = final_elf.with_extension("bin");
        eprintln!(
            "[fstart] objcopy: {} -> {}",
            final_elf.display(),
            bin_path.display()
        );
        let objcopy_status = Command::new("llvm-objcopy")
            .arg("-O")
            .arg("binary")
            .arg("--remove-section=.bss")
            .arg(&final_elf)
            .arg(&bin_path)
            .status()
            .map_err(|e| format!("failed to run llvm-objcopy: {e}"))?;
        if !objcopy_status.success() {
            return Err("llvm-objcopy failed".to_string());
        }
        bin_path
    } else {
        final_elf.clone()
    };

    eprintln!("[fstart] built: {}", run_path.display());
    Ok((final_elf, run_path))
}

/// Public wrapper for workspace root (used by other xtask modules).
pub fn workspace_root_pub() -> Result<PathBuf, String> {
    workspace_root()
}

fn workspace_root() -> Result<PathBuf, String> {
    // Walk up from current dir looking for the workspace Cargo.toml
    let mut dir = std::env::current_dir().map_err(|e| format!("no cwd: {e}"))?;
    loop {
        let cargo_toml = dir.join("Cargo.toml");
        if cargo_toml.exists() {
            let contents =
                std::fs::read_to_string(&cargo_toml).map_err(|e| format!("read error: {e}"))?;
            if contents.contains("[workspace]") {
                return Ok(dir);
            }
        }
        if !dir.pop() {
            return Err("could not find workspace root".to_string());
        }
    }
}
