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
    manifest_path: Option<PathBuf>,
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
    build_info: fstart_types::BuildInfo,
    parsed: &fstart_codegen::board_loader::ParsedBoard,
    release: bool,
) -> Result<BuildResult, String> {
    let config = &parsed.config;

    eprintln!("[fstart] board: {}", config.name);
    eprintln!("[fstart] platform: {}", config.platform);
    eprintln!("[fstart] board package: {}", build_info.board_package);

    let smm_artifacts = build_smm_artifacts(workspace_root, config.name.as_str(), release, config)?;
    let plan = crate::build_plan::plan(parsed, &build_info)?;

    eprintln!("[fstart] target: {}", build_info.target);

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
    board_name: &str,
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
    let linker_script = fstart_codegen::linker::generate_linker_script(config, stage_name);
    std::fs::write(&link_ld, linker_script)
        .map_err(|e| format!("failed to write {}: {e}", link_ld.display()))?;
    let metadata = format!(
        "board_source=rust:{}\nstage={stage_label}\nprofile={profile}\ntarget={target}\nfeatures={features}\nlinker_script={}\n",
        board_manifest.board,
        link_ld.display()
    );
    std::fs::write(artifact_dir.join("metadata.txt"), metadata)
        .map_err(|e| format!("failed to write stage metadata: {e}"))?;

    let mut cmd = Command::new("cargo");
    cmd.current_dir(workspace_root);
    cmd.arg("build");
    if let Some(manifest_path) = &stage_package.manifest_path {
        cmd.arg("--manifest-path").arg(manifest_path);
        cmd.env("CARGO_TARGET_DIR", workspace_root.join("target"));
    } else {
        cmd.arg("--package").arg(&stage_package.package_label);
    }
    cmd.arg("--bin")
        .arg(stage_bin)
        .arg("--target")
        .arg(target)
        .arg("--no-default-features")
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

    // Pass board/stage context to build.rs. Board-aware planning already
    // happened here; fstart-stage/build.rs only forwards link.ld to rustc.
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
    workspace_root: &Path,
    board_manifest: &crate::board_manifest::BoardManifest,
    config: &fstart_types::BoardConfig,
    plan: &crate::build_plan::BuildPlan,
) -> Result<StagePackageBuild, String> {
    if let Some(stage_package) = &board_manifest.stage_package {
        return Ok(StagePackageBuild {
            package_label: stage_package.clone(),
            manifest_path: None,
        });
    }

    let recipe = board_manifest.stage_recipe.as_deref().ok_or_else(|| {
        format!(
            "board '{}' does not declare package.metadata.fstart.stage-package or stage-recipe",
            board_manifest.board
        )
    })?;

    // Every recipe is a FirmwareBoard/StageRecipe recipe: the board crate's
    // `stage` feature pulls its platform recipe crate, so the wrapper needs
    // no recipe-specific knowledge.
    let _ = recipe;
    write_firmware_board_stage_wrapper(workspace_root, board_manifest, config, plan)
}

fn board_feature_names(manifest_path: &Path) -> Vec<String> {
    let Ok(text) = fs::read_to_string(manifest_path) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    let mut in_features = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed == "[features]" {
            in_features = true;
            continue;
        }
        if in_features && trimmed.starts_with('[') {
            break;
        }
        if !in_features || trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((name, _)) = trimmed.split_once('=') {
            names.push(name.trim().to_string());
        }
    }
    names
}

/// Generate a selected-board stage wrapper for any `FirmwareBoard`/`StageRecipe`
/// recipe. The wrapper is pure build glue: it selects the board type and calls
/// `fstart_stage::run_board::<Board>`. Recipe-specific sequencing lives in the
/// board's platform recipe crate, pulled in transitively by the board crate's
/// `stage` feature. No recipe-specific knowledge lives here — adding a new
/// FirmwareBoard recipe needs zero xtask changes.
fn write_firmware_board_stage_wrapper(
    workspace_root: &Path,
    board_manifest: &crate::board_manifest::BoardManifest,
    config: &fstart_types::BoardConfig,
    plan: &crate::build_plan::BuildPlan,
) -> Result<StagePackageBuild, String> {
    let package_label = format!("fstart-selected-stage-{}", board_manifest.board);
    let wrapper_dir = workspace_root
        .join("target")
        .join("fstart-build")
        .join(&board_manifest.board)
        .join("selected-stage");
    let src_dir = wrapper_dir.join("src");
    fs::create_dir_all(&src_dir)
        .map_err(|e| format!("failed to create {}: {e}", src_dir.display()))?;

    let cargo_toml = selected_firmware_board_cargo_toml(workspace_root, board_manifest, plan);
    write_if_changed(&wrapper_dir.join("Cargo.toml"), &cargo_toml)?;
    if let Ok(lockfile) = fs::read_to_string(workspace_root.join("Cargo.lock")) {
        write_if_changed(&wrapper_dir.join("Cargo.lock"), &lockfile)?;
    }
    write_if_changed(
        &wrapper_dir.join("build.rs"),
        selected_firmware_board_build_rs(),
    )?;
    let (bootblock_heap, main_heap) = stage_heap_sizes(config);
    write_if_changed(
        &src_dir.join("main.rs"),
        &selected_firmware_board_main_rs(bootblock_heap, main_heap),
    )?;

    Ok(StagePackageBuild {
        package_label,
        manifest_path: Some(wrapper_dir.join("Cargo.toml")),
    })
}

/// Build the wrapper `Cargo.toml`. Its `[features]` table forwards board
/// features to the board crate, and only true stage flow/backend/build features
/// to `fstart-stage`. Platform/driver recipe feature names may still appear in
/// the generated wrapper so Cargo accepts the build-plan feature list, but they
/// are not routed through a central stage driver registry.
fn selected_firmware_board_cargo_toml(
    workspace_root: &Path,
    board_manifest: &crate::board_manifest::BoardManifest,
    plan: &crate::build_plan::BuildPlan,
) -> String {
    let board_path = path_for_toml(&board_manifest.dir);
    let crate_path = |path: &str| path_for_toml(&workspace_root.join(path));

    let board_features: std::collections::BTreeSet<String> =
        board_feature_names(&board_manifest.dir.join("Cargo.toml"))
            .into_iter()
            .filter(|f| f != "default" && f != "stage")
            .collect();
    let plan_features: std::collections::BTreeSet<String> = plan
        .stages
        .iter()
        .flat_map(|stage| stage.features.iter().map(str::to_owned))
        .collect();

    let mut feature_lines = String::new();
    let mut all_features = std::collections::BTreeSet::new();
    all_features.extend(board_features.iter().cloned());
    all_features.extend(plan_features.iter().cloned());
    for feature in &all_features {
        let mut deps = Vec::new();
        if board_features.contains(feature.as_str()) {
            deps.push(format!("\"fstart-board-selected/{feature}\""));
        }
        if plan_features.contains(feature.as_str()) && firmware_wrapper_stage_feature(feature) {
            deps.push(format!("\"fstart-stage/{feature}\""));
        }
        feature_lines.push_str(&format!("{feature} = [{}]\n", deps.join(", ")));
    }

    format!(
        r#"[package]
name = "{package_label}"
version = "0.0.0"
edition = "2021"
publish = false

[workspace]

[features]
default = []
{feature_lines}
[dependencies]
fstart-board-selected = {{ package = "{board_package}", path = "{board_path}", default-features = false, features = ["stage"] }}
fstart-stage = {{ path = "{stage_path}" }}
fstart-platform-x86_64 = {{ path = "{x86_platform_path}" }}
fstart-types = {{ path = "{types_path}" }}
ufmt = {{ version = "0.2", default-features = false }}

[[bin]]
name = "fstart-stage"
path = "src/main.rs"
"#,
        package_label = format!("fstart-selected-stage-{}", board_manifest.board),
        board_package = board_manifest.package,
        stage_path = crate_path("crates/fstart-stage"),
        x86_platform_path = crate_path("crates/fstart-platform-x86_64"),
        types_path = crate_path("crates/fstart-types"),
    )
}

fn firmware_wrapper_stage_feature(feature: &str) -> bool {
    feature.starts_with("flow-profile-")
        || matches!(
            feature,
            "flow-security"
                | "ffs"
                | "ed25519"
                | "sha2-digest"
                | "sha3-digest"
                | "lz4"
                | "fit"
                | "fdt"
                | "handoff"
                | "acpi"
                | "acpi-load"
                | "smbios"
                | "memory-detect"
                | "crabefi"
                | "x86_64"
                | "x86-boot"
                | "x86-1g-pages"
                | "x86-writable-page-tables"
                | "x86-static-page-tables"
                | "ns16550"
                | "ns16550-pio"
        )
}

/// Wrapper `main.rs`: selects the board type and dispatches to its recipe via
/// `fstart_stage::run_board`. Heap sizes are inlined from the board's stage
/// config; the `fstart_stage_bootblock` cfg selects between them.
fn selected_firmware_board_main_rs(bootblock_heap: usize, main_heap: usize) -> String {
    format!(
        r#"//! Generated selected-board FirmwareBoard stage wrapper.

#![no_std]
#![no_main]

extern crate fstart_platform_x86_64 as fstart_platform;
extern crate ufmt;

use fstart_board_selected::Board;

#[cfg(fstart_stage_bootblock)]
const HEAP_SIZE: usize = {bootblock_heap};
#[cfg(not(fstart_stage_bootblock))]
const HEAP_SIZE: usize = {main_heap};

#[repr(align(16))]
#[allow(dead_code)]
struct HeapStore([u8; HEAP_SIZE]);

#[no_mangle]
static mut _FSTART_HEAP: HeapStore = HeapStore([0; HEAP_SIZE]);

#[no_mangle]
static _FSTART_HEAP_SIZE: usize = HEAP_SIZE;

#[no_mangle]
static _fstart_anchor_early: fstart_types::ffs::AnchorBlock =
    fstart_types::ffs::AnchorBlock::placeholder();

#[cfg(fstart_stage_bootblock)]
#[no_mangle]
static _fstart_early_microcode_enabled: u32 = 1;
#[cfg(not(fstart_stage_bootblock))]
#[no_mangle]
static _fstart_early_microcode_enabled: u32 = 0;

#[no_mangle]
pub extern "Rust" fn fstart_main(handoff_ptr: usize) -> ! {{
    fstart_stage::run_board::<Board>(
        fstart_stage::StageKind::from_option(option_env!("FSTART_STAGE_NAME")),
        handoff_ptr,
    )
}}

#[used]
#[cfg_attr(target_os = "none", link_section = ".fstart.keep")]
static FSTART_MAIN_KEEP: extern "Rust" fn(usize) -> ! = fstart_main;
"#
    )
}

/// Build script shared by all FirmwareBoard recipe wrappers: forwards the
/// linker script, selects the bootblock cfg, and wires the optional SMM image.
fn selected_firmware_board_build_rs() -> &'static str {
    r#"use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rustc-check-cfg=cfg(fstart_stage_bootblock)");
    println!("cargo:rerun-if-env-changed=FSTART_LINKER_SCRIPT");
    println!("cargo:rerun-if-env-changed=FSTART_STAGE_NAME");
    println!("cargo:rerun-if-env-changed=FSTART_SMM_IMAGE");
    println!("cargo:rerun-if-env-changed=FSTART_SMM_COREBOOT_HEADER");

    if let Ok(script) = env::var("FSTART_LINKER_SCRIPT") {
        println!("cargo:rustc-link-arg-bin=fstart-stage=-T{script}");
        println!("cargo:rerun-if-changed={script}");
    }

    if env::var("FSTART_STAGE_NAME").as_deref() == Ok("bootblock") {
        println!("cargo:rustc-cfg=fstart_stage_bootblock");
    }

    if let Ok(image) = env::var("FSTART_SMM_IMAGE") {
        println!("cargo:rustc-env=FSTART_SMM_IMAGE={image}");
        println!("cargo:rerun-if-changed={image}");
    } else {
        let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR set by Cargo"));
        let empty = out.join("empty-smm.bin");
        fs::write(&empty, []).expect("write empty SMM image placeholder");
        println!("cargo:rustc-env=FSTART_SMM_IMAGE={}", empty.display());
    }

    if let Ok(header) = env::var("FSTART_SMM_COREBOOT_HEADER") {
        println!("cargo:rustc-env=FSTART_SMM_COREBOOT_HEADER={header}");
        println!("cargo:rerun-if-changed={header}");
    }
}
"#
}

/// Return `(bootblock_heap, main_heap)` for a board's stage layout, used to
/// size the wrapper's `_FSTART_HEAP` static per stage. Falls back to small
/// defaults when a stage omits `heap_size`.
fn stage_heap_sizes(config: &fstart_types::BoardConfig) -> (usize, usize) {
    use fstart_types::StageLayout;
    let stages = match &config.stages {
        StageLayout::MultiStage(stages) => stages.iter(),
        StageLayout::Monolithic(_) => return (0x100, 0x200000),
    };
    let bootblock = stages
        .clone()
        .find(|s| s.name.as_str() == "bootblock")
        .or_else(|| stages.clone().next());
    let main = stages
        .clone()
        .find(|s| s.name.as_str() != "bootblock")
        .or_else(|| stages.clone().last());
    (
        bootblock.and_then(|s| s.heap_size).unwrap_or(0x100) as usize,
        main.and_then(|s| s.heap_size).unwrap_or(0x200000) as usize,
    )
}

fn write_if_changed(path: &Path, content: &str) -> Result<(), String> {
    if fs::read_to_string(path).is_ok_and(|existing| existing == content) {
        return Ok(());
    }
    fs::write(path, content).map_err(|e| format!("failed to write {}: {e}", path.display()))
}

fn path_for_toml(path: &Path) -> String {
    path.display().to_string().replace('\\', "\\\\")
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

#[cfg(test)]
mod tests {
    use super::firmware_wrapper_stage_feature;

    #[test]
    fn build_board_firmware_wrapper_stage_feature_allowlist_excludes_recipe_drivers() {
        for feature in [
            "flow-profile-multistage",
            "flow-security",
            "ffs",
            "sha2-digest",
            "crabefi",
            "x86_64",
            "x86-boot",
            "x86-static-page-tables",
            "ns16550-pio",
        ] {
            assert!(
                firmware_wrapper_stage_feature(feature),
                "{feature} should route to fstart-stage"
            );
        }

        for feature in [
            "intel-gm965",
            "intel-ich8",
            "intel-pineview",
            "intel-ich7",
            "i2c-ck505",
            "ite8721f",
            "nsc-pc87392",
            "q35-hostbridge",
            "qemu-fw-cfg",
            "pci-ecam",
            "cpu-intel-core2",
            "mp",
        ] {
            assert!(
                !firmware_wrapper_stage_feature(feature),
                "{feature} should stay with the selected board/recipe"
            );
        }
    }
}
