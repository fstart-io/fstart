//! Board build orchestration.
//!
//! 1. Parse board.ron
//! 2. Determine target triple, cargo features, and environment
//! 3. Invoke cargo build on fstart-stage (once for monolithic, per-stage for multi-stage)
//! 4. Return the path(s) to the built binary(ies)

use fstart_codegen::ron_loader;
use fstart_types::{Capability, SocImageFormat, StageLayout};
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

/// Generated standalone SMM artifacts for a board build.
#[derive(Debug, Clone)]
struct SmmArtifacts {
    /// Native PIC SMM image consumed by fstart/coreboot loaders.
    image_path: PathBuf,
    /// Optional coreboot-compatible generated offsets header.
    header_path: Option<PathBuf>,
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
    let parsed = ron_loader::load_parsed_board(&board_ron)?;
    let config = &parsed.config;

    eprintln!("[fstart] board: {}", config.name);
    eprintln!("[fstart] platform: {}", config.platform);
    eprintln!("[fstart] mode: {:?}", config.mode);

    let smm_artifacts = build_smm_artifacts(&workspace_root, board_name, release, config)?;
    let plan = crate::build_plan::plan(&parsed);

    eprintln!("[fstart] target: {}", plan.target.triple);

    let mut result = Vec::new();
    for stage in &plan.stages {
        if stage.stage_name.is_some() {
            eprintln!("[fstart] building stage: {}", stage.display_name);
        }
        let features = stage.features_arg();
        eprintln!("[fstart] features: {features}");

        let (elf_path, run_path) = build_one_stage(
            &workspace_root,
            &board_ron,
            stage.stage_name.as_deref(),
            plan.target.triple,
            &features,
            release,
            stage.needs_flat_binary,
            stage.build_std,
            stage.soc_format,
            smm_artifacts.as_ref(),
        )?;
        result.push(StageBinary {
            name: stage.display_name.clone(),
            path: elf_path,
            run_path,
            load_addr: stage.load_addr,
        });
    }

    Ok(BuildResult { stages: result })
}
/// Build the standalone SMM image artifacts requested by the board.
fn build_smm_artifacts(
    workspace_root: &std::path::Path,
    board_name: &str,
    release: bool,
    config: &fstart_types::BoardConfig,
) -> Result<Option<SmmArtifacts>, String> {
    let Some(smm) = config.smm else {
        return Ok(None);
    };

    let smm_num_cpus = max_smm_num_cpus(&config.stages);
    let entry_count = smm.entry_points.or(smm_num_cpus).unwrap_or(1);
    if let Some(num_cpus) = smm_num_cpus {
        if entry_count < num_cpus {
            return Err(
                "board.smm.entry_points must be greater than or equal to MpInit.num_cpus"
                    .to_string(),
            );
        }
    }

    let profile = if release { "release" } else { "debug" };
    let out_dir = workspace_root
        .join("target")
        .join("smm")
        .join(board_name)
        .join(profile);
    let image_path = out_dir.join("fstart-smm.bin");
    let header_path = smm
        .coreboot
        .emit_header
        .then(|| out_dir.join("fstart_smm_offsets.h"));

    let options = fstart_smm_image::ImageOptions {
        entry_count,
        stack_size: smm.stack_size,
        coreboot_module_args: smm.coreboot.module_args,
        coreboot_header: smm.coreboot.emit_header,
        platform: smm.platform,
    };
    let built = fstart_smm_image::write_image(options, &image_path, header_path.as_deref())
        .map_err(|e| format!("failed to build SMM image: {e}"))?;

    eprintln!(
        "[fstart] SMM image: {} ({} bytes, {} entries)",
        image_path.display(),
        built.image.len(),
        entry_count
    );
    if let Some(path) = &header_path {
        eprintln!("[fstart] SMM coreboot header: {}", path.display());
    }

    Ok(Some(SmmArtifacts {
        image_path,
        header_path,
    }))
}

fn max_smm_num_cpus(stages: &StageLayout) -> Option<u16> {
    fn max_in_caps(caps: &[Capability]) -> Option<u16> {
        caps.iter()
            .filter_map(|cap| match cap {
                Capability::MpInit {
                    num_cpus,
                    smm: true,
                    ..
                } => Some(*num_cpus),
                _ => None,
            })
            .max()
    }

    match stages {
        StageLayout::Monolithic(stage) => max_in_caps(&stage.capabilities),
        StageLayout::MultiStage(stages) => stages
            .iter()
            .filter_map(|stage| max_in_caps(&stage.capabilities))
            .max(),
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
    soc_format: SocImageFormat,
    smm_artifacts: Option<&SmmArtifacts>,
) -> Result<(PathBuf, PathBuf), String> {
    let profile = if release { "release" } else { "debug" };
    let board_label = board_ron
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("board");
    let stage_label = stage_name.unwrap_or("stage");
    let artifact_dir = workspace_root
        .join("target")
        .join("fstart-generated")
        .join(board_label)
        .join(profile)
        .join(stage_label);

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

    let mut rustflags = crate::toolchain::rustflags_for_triple(target);
    // FSTART_EXTRA_RUSTFLAGS (if set) is appended — CI uses this for
    // -Dwarnings to catch generated-code regressions.
    if let Ok(extra) = std::env::var("FSTART_EXTRA_RUSTFLAGS") {
        rustflags.push(' ');
        rustflags.push_str(&extra);
    }
    cmd.env("RUSTFLAGS", &rustflags);

    // Pass board/stage context to build.rs.  FSTART_STAGE_ARTIFACT_DIR
    // mirrors generated_stage.rs/link.ld to a stable, human-readable path;
    // Cargo's OUT_DIR remains the canonical path used by include!/linking.
    cmd.env("FSTART_BOARD_RON", board_ron.to_str().unwrap());
    cmd.env("FSTART_STAGE_ARTIFACT_DIR", &artifact_dir);
    cmd.env("FSTART_STAGE_FEATURES", features);
    if let Some(name) = stage_name {
        cmd.env("FSTART_STAGE_NAME", name);
    }
    if let Some(smm) = smm_artifacts {
        cmd.env("FSTART_SMM_IMAGE", &smm.image_path);
        if let Some(header) = &smm.header_path {
            cmd.env("FSTART_SMM_COREBOOT_HEADER", header);
        }
    }

    eprintln!("[fstart] generated artifacts: {}", artifact_dir.display());
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

    // Determine output binary path.
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

        // Allwinner eGON: compute the actual binary size, pad to
        // 512-byte alignment, and patch both length and checksum.
        if let SocImageFormat::AllwinnerEgon = soc_format {
            crate::image::egon::patch_file(&bin_path)?;
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
