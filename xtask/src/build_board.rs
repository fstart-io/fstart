//! Board build orchestration.
//!
//! 1. Ask the Rust board crate for metadata
//! 2. Determine target triple, cargo features, and environment
//! 3. Invoke cargo build on the board-owned stage binary
//! 4. Return the path(s) to the built binary(ies)

use fstart_types::{Capability, SocImageFormat, StageLayout};
use object::elf;
use object::read::elf::{ElfFile, FileHeader, ProgramHeader};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

struct StagePackageBuild {
    package_label: String,
}

struct SmmStageBuild {
    archive_path: PathBuf,
    link_dir: PathBuf,
}

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
    /// Path to the ELF binary on disk (used by assembler diagnostics/packaging).
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
    let _ = release;
    Err(format!(
        "board '{board_name}' must be built through its board-owned host tool"
    ))
}

/// Build firmware from already-loaded Rust board metadata.
pub fn build_with_parsed(
    workspace_root: &Path,
    board_manifest: &crate::board_manifest::BoardManifest,
    parsed: &crate::build_plan::ParsedBoard,
    release: bool,
) -> Result<BuildResult, String> {
    let config = &parsed.config;

    eprintln!("[fstart] board: {}", config.name);
    eprintln!("[fstart] platform: {}", config.platform);
    eprintln!("[fstart] board package: {}", board_manifest.package);

    let smm_artifacts = build_smm_artifacts(workspace_root, board_manifest, release, config)?;
    let plan = crate::build_plan::plan(parsed, board_manifest)?;

    eprintln!("[fstart] target: {}", plan.target.triple);

    let mut result = Vec::new();
    let stage_package = stage_package_build(workspace_root, board_manifest, config, &plan)?;
    for stage in &plan.stages {
        if stage.stage_name.is_some() {
            eprintln!("[fstart] building stage: {}", stage.display_name);
        }
        let features = stage.features_arg();
        eprintln!("[fstart] features: {features}");

        let (elf_path, run_path) = build_one_stage(
            workspace_root,
            board_manifest,
            config,
            &stage_package,
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
    board_manifest: &crate::board_manifest::BoardManifest,
    release: bool,
    config: &fstart_types::BoardConfig,
) -> Result<Option<SmmArtifacts>, String> {
    let Some(smm) = config.smm else {
        return Ok(None);
    };

    let smm_max_cpus = max_smm_cpus(&config.stages);
    let entry_count = smm.entry_points.or(smm_max_cpus).unwrap_or(1);
    if let Some(max_cpus) = smm_max_cpus {
        if entry_count < max_cpus {
            return Err(
                "board.smm.entry_points must be greater than or equal to MpInit.max_cpus"
                    .to_string(),
            );
        }
    }

    let profile = if release { "release" } else { "debug" };
    let out_dir = workspace_root
        .join("target")
        .join("smm")
        .join(&board_manifest.board)
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
    };
    let smm_stage = build_board_smm_stage(workspace_root, board_manifest)?;
    let handler =
        fstart_smm_image::handler_from_archive(&smm_stage.archive_path, &smm_stage.link_dir)
            .map_err(|e| format!("failed to link SMM handler: {e}"))?;
    let built =
        fstart_smm_image::write_image(options, &handler, &image_path, header_path.as_deref())
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

fn build_board_smm_stage(
    workspace_root: &Path,
    board_manifest: &crate::board_manifest::BoardManifest,
) -> Result<SmmStageBuild, String> {
    let target_dir = workspace_root
        .join("target")
        .join("smm")
        .join(&board_manifest.board)
        .join("cargo");

    let mut cmd = Command::new("cargo");
    cmd.current_dir(workspace_root)
        .arg("rustc")
        .arg("--package")
        .arg(&board_manifest.package)
        .arg("--lib")
        .arg("--target")
        .arg("x86_64-unknown-none")
        .arg("--target-dir")
        .arg(&target_dir)
        .arg("--no-default-features")
        .arg("--features")
        .arg("smm")
        .arg("--release")
        .arg("--")
        .arg("-C")
        .arg("panic=abort")
        .arg("-C")
        .arg("opt-level=s")
        .arg("-C")
        .arg("relocation-model=pic")
        .arg("-C")
        .arg("no-redzone=yes");

    eprintln!(
        "[fstart] building board SMM stage: {}...",
        board_manifest.package
    );
    let status = cmd
        .status()
        .map_err(|e| format!("failed to run cargo for SMM stage: {e}"))?;
    if !status.success() {
        return Err("board SMM stage build failed".to_string());
    }

    Ok(SmmStageBuild {
        archive_path: target_dir
            .join("x86_64-unknown-none")
            .join("release")
            .join(format!("lib{}.a", board_manifest.package.replace('-', "_"))),
        link_dir: workspace_root
            .join("target")
            .join("smm")
            .join(&board_manifest.board)
            .join("link"),
    })
}

fn max_smm_cpus(stages: &StageLayout) -> Option<u16> {
    fn max_in_caps(caps: &[Capability]) -> Option<u16> {
        caps.iter()
            .filter_map(|cap| match cap {
                Capability::MpInit {
                    max_cpus,
                    smm: true,
                } => Some(*max_cpus),
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

/// Build a single board-owned stage binary.
///
/// `stage_name` is `None` for monolithic, `Some("bootblock")` etc. for multi-stage.
#[allow(clippy::too_many_arguments)]
/// Returns (elf_path, run_path). For AArch64 and RISC-V these differ
/// (ELF vs flat binary for QEMU); for other platforms they are the same.
fn build_one_stage(
    workspace_root: &std::path::Path,
    board_manifest: &crate::board_manifest::BoardManifest,
    config: &fstart_types::BoardConfig,
    stage_package: &StagePackageBuild,
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
    let stage_bin = board_manifest.stage_bin.as_deref().ok_or_else(|| {
        format!(
            "board '{}' does not declare package.metadata.fstart.stage-bin; static stage adapter is not implemented",
            board_manifest.board
        )
    })?;
    let board_label = board_manifest.board.as_str();
    let stage_label = stage_name.unwrap_or("stage");
    let artifact_dir = workspace_root
        .join("target")
        .join("fstart-build")
        .join(board_label)
        .join(profile)
        .join(stage_label);
    std::fs::create_dir_all(&artifact_dir)
        .map_err(|e| format!("failed to create stage artifact dir: {e}"))?;
    let link_ld = artifact_dir.join("link.ld");
    let linker_script = crate::linker::generate_linker_script(config, stage_name);
    std::fs::write(&link_ld, linker_script)
        .map_err(|e| format!("failed to write {}: {e}", link_ld.display()))?;
    let metadata = format!(
        "board_source=rust:{}\nstage={stage_label}\nprofile={profile}\ntarget={target}\nfeatures={features}\nlinker_script={}\n",
        board_manifest.board,
        link_ld.display()
    );
    std::fs::write(artifact_dir.join("metadata.txt"), metadata)
        .map_err(|e| format!("failed to write stage metadata: {e}"))?;

    let features = if features.is_empty() {
        "stage".to_string()
    } else {
        format!("stage,{features}")
    };

    let mut cmd = Command::new("cargo");
    cmd.current_dir(workspace_root);
    cmd.arg("build");
    cmd.arg("--package").arg(&stage_package.package_label);
    cmd.arg("--bin")
        .arg(stage_bin)
        .arg("--target")
        .arg(target)
        .arg("--no-default-features")
        .arg("--features")
        .arg(&features)
        .arg("-Z")
        .arg(format!("build-std={build_std}"));

    if release {
        cmd.arg("--release");
    }

    let mut rustflags = crate::toolchain::rustflags_for_triple(target);
    rustflags.push_str(" -Clink-arg=-T");
    rustflags.push_str(&link_ld.display().to_string());
    // FSTART_EXTRA_RUSTFLAGS (if set) is appended — CI uses this for
    // -Dwarnings to catch generated-code regressions.
    if let Ok(extra) = std::env::var("FSTART_EXTRA_RUSTFLAGS") {
        rustflags.push(' ');
        rustflags.push_str(&extra);
    }
    cmd.env("RUSTFLAGS", &rustflags);

    // Pass board/stage context to build scripts and the selected board crate.
    // fstart-stage/build.rs forwards the generated linker script.
    cmd.env("FSTART_RUST_BOARD", &board_manifest.board);
    cmd.env("FSTART_LINKER_SCRIPT", &link_ld);
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

    eprintln!("[fstart] build artifacts: {}", artifact_dir.display());
    eprintln!(
        "[fstart] building {}:{}...",
        stage_package.package_label, stage_bin
    );
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
        .join(stage_bin);

    // For multi-stage: copy the binary to a stage-specific name so subsequent
    // builds don't overwrite it (cargo always outputs to the selected stage-bin name).
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
    // is NOLOAD and its VMA is in RAM, which would cause naive flat extraction to span
    // the ROM→RAM gap (producing a multi-GiB file of mostly zeros). The
    // entry code clears BSS at runtime.
    let run_path = if needs_flat_binary {
        let bin_path = final_elf.with_extension("bin");
        eprintln!(
            "[fstart] elf-to-bin: {} -> {}",
            final_elf.display(),
            bin_path.display()
        );
        write_flat_binary(&final_elf, &bin_path)?;

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

fn stage_package_build(
    _workspace_root: &Path,
    board_manifest: &crate::board_manifest::BoardManifest,
    _config: &fstart_types::BoardConfig,
    _plan: &crate::build_plan::BuildPlan,
) -> Result<StagePackageBuild, String> {
    Ok(StagePackageBuild {
        package_label: board_manifest
            .stage_package
            .clone()
            .unwrap_or_else(|| board_manifest.package.clone()),
    })
}

/// Public wrapper for workspace root (used by other xtask modules).
pub fn workspace_root_pub() -> Result<PathBuf, String> {
    workspace_root()
}

fn write_flat_binary(elf_path: &Path, bin_path: &Path) -> Result<(), String> {
    let elf_data = fs::read(elf_path)
        .map_err(|e| format!("failed to read ELF {}: {e}", elf_path.display()))?;
    let load_segments = elf_load_segments(&elf_data, elf_path)?;

    let min_paddr = load_segments
        .iter()
        .filter(|segment| segment.filesz != 0)
        .map(|segment| segment.paddr)
        .min()
        .ok_or_else(|| format!("no loadable file-backed segments in {}", elf_path.display()))?;
    let max_paddr = load_segments
        .iter()
        .filter(|segment| segment.filesz != 0)
        .map(|segment| segment.paddr.saturating_add(segment.filesz))
        .max()
        .unwrap_or(min_paddr);
    let len = max_paddr
        .checked_sub(min_paddr)
        .and_then(|len| usize::try_from(len).ok())
        .ok_or_else(|| {
            format!(
                "flat binary address range is too large in {}",
                elf_path.display()
            )
        })?;

    let mut flat = vec![0u8; len];
    for segment in load_segments.iter().filter(|segment| segment.filesz != 0) {
        let start = usize::try_from(segment.paddr - min_paddr)
            .map_err(|_| format!("segment offset is too large in {}", elf_path.display()))?;
        let size = usize::try_from(segment.filesz)
            .map_err(|_| format!("segment size is too large in {}", elf_path.display()))?;
        let file_start = usize::try_from(segment.offset)
            .map_err(|_| format!("segment file offset is too large in {}", elf_path.display()))?;
        let file_end = file_start
            .checked_add(size)
            .ok_or_else(|| format!("segment file range overflows in {}", elf_path.display()))?;
        if file_end > elf_data.len() {
            return Err(format!(
                "PT_LOAD at {:#x} extends past EOF in {}",
                segment.paddr,
                elf_path.display()
            ));
        }
        flat[start..start + size].copy_from_slice(&elf_data[file_start..file_end]);
    }

    fs::write(bin_path, flat)
        .map_err(|e| format!("failed to write flat binary {}: {e}", bin_path.display()))
}

#[derive(Debug, Clone, Copy)]
struct ElfLoadSegment {
    offset: u64,
    paddr: u64,
    filesz: u64,
}

fn elf_load_segments(elf_data: &[u8], elf_path: &Path) -> Result<Vec<ElfLoadSegment>, String> {
    if elf_data.len() < 16 {
        return Err(format!("ELF {} is too short", elf_path.display()));
    }

    match elf_data[4] {
        elf::ELFCLASS32 => elf_load_segments_for::<elf::FileHeader32<object::Endianness>>(elf_data),
        elf::ELFCLASS64 => elf_load_segments_for::<elf::FileHeader64<object::Endianness>>(elf_data),
        class => {
            return Err(format!(
                "unsupported ELF class {class} in {}",
                elf_path.display()
            ));
        }
    }
    .map_err(|e| format!("failed to parse ELF {}: {e}", elf_path.display()))
}

fn elf_load_segments_for<Elf>(elf_data: &[u8]) -> Result<Vec<ElfLoadSegment>, object::Error>
where
    Elf: FileHeader,
{
    let elf = ElfFile::<Elf>::parse(elf_data)?;
    let endian = elf.elf_header().endian()?;
    Ok(elf
        .elf_program_headers()
        .iter()
        .filter(|phdr| phdr.p_type(endian) == elf::PT_LOAD)
        .map(|phdr| ElfLoadSegment {
            offset: phdr.p_offset(endian).into(),
            paddr: phdr.p_paddr(endian).into(),
            filesz: phdr.p_filesz(endian).into(),
        })
        .collect())
}

fn workspace_root() -> Result<PathBuf, String> {
    if let Ok(root) = std::env::var("FSTART_WORKSPACE_ROOT") {
        return Ok(PathBuf::from(root));
    }

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
