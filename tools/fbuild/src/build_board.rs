use fstart_core::{SocImageFormat, StageLayout};
use object::elf;
use object::read::elf::{ElfFile, FileHeader, ProgramHeader};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(test)]
#[path = "layout_linker_tests.rs"]
mod layout_linker_tests;

struct SmmStageBuild {
    deps_dir: PathBuf,
    link_dir: PathBuf,
}

pub struct BuildResult {
    pub stages: Vec<fstart_image_build::StageBinary>,
}

#[derive(Debug, Clone)]
pub(crate) struct SmmArtifacts {
    pub image_path: PathBuf,
    pub header_path: Option<PathBuf>,
}

impl BuildResult {
    pub fn primary_binary(&self) -> &fstart_image_build::StageBinary {
        &self.stages[0]
    }
}

pub fn build_with_parsed(
    workspace_root: &Path,
    board_manifest: &crate::board_manifest::BoardManifest,
    parsed: &crate::build_plan::ParsedBoard,
    release: bool,
) -> Result<BuildResult, String> {
    if let Some(resolved) = &parsed.resolved {
        return resolved.build(workspace_root, board_manifest, release);
    }
    let config = &parsed.config;

    eprintln!("[fstart] board: {}", config.name);
    eprintln!("[fstart] platform: {}", config.platform);
    eprintln!("[fstart] board package: {}", board_manifest.package);

    let smm_artifacts = build_smm_artifacts(workspace_root, board_manifest, release, config, None)?;
    let plan = crate::build_plan::plan(parsed, board_manifest)?;

    eprintln!("[fstart] target: {}", plan.target.triple);

    let mut result = Vec::new();
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
            stage.stage_name.as_deref(),
            stage.stage_env,
            plan.target.triple,
            &features,
            release,
            stage.needs_flat_binary,
            stage.build_std,
            stage.soc_format,
            smm_artifacts.as_ref(),
        )?;
        result.push(fstart_image_build::StageBinary {
            name: stage.display_name.clone(),
            path: elf_path,
            run_path,
            load_addr: stage.load_addr,
        });
    }

    Ok(BuildResult { stages: result })
}
pub(crate) fn build_smm_artifacts(
    workspace_root: &std::path::Path,
    board_manifest: &crate::board_manifest::BoardManifest,
    release: bool,
    config: &fstart_core::BoardConfig,
    selected_features: Option<&[String]>,
) -> Result<Option<SmmArtifacts>, String> {
    let Some(smm) = config.smm else {
        return Ok(None);
    };

    let smm_max_cpus = max_smm_cpus(&config.stages);
    let entry_count = smm.entry_points.or(smm_max_cpus).unwrap_or(1);
    if let Some(max_cpus) = smm_max_cpus
        && entry_count < max_cpus
    {
        return Err(
            "board.smm.entry_points must be greater than or equal to stage MP max_cpus".to_string(),
        );
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

    let options = fstart_image_build::smm_image::ImageOptions {
        entry_count,
        stack_size: smm.stack_size,
        coreboot_module_args: smm.coreboot.module_args,
        coreboot_header: smm.coreboot.emit_header,
    };
    let smm_stage = build_board_smm_stage(workspace_root, board_manifest, selected_features)?;
    let handler =
        fstart_image_build::smm_image::handler_from_rlibs(&smm_stage.deps_dir, &smm_stage.link_dir)
            .map_err(|e| format!("failed to link SMM handler: {e}"))?;
    let built = fstart_image_build::smm_image::write_image(
        options,
        &handler,
        &image_path,
        header_path.as_deref(),
    )
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
    selected_features: Option<&[String]>,
) -> Result<SmmStageBuild, String> {
    let target_dir = workspace_root
        .join("target")
        .join("smm")
        .join(&board_manifest.board)
        .join("cargo");

    let selected_workspace = prepare_selected_board_workspace(workspace_root, board_manifest)?;

    let smm_features = selected_features.map_or_else(
        || {
            std::iter::once("smm".to_owned())
                .chain(board_manifest.variant_features.iter().cloned())
                .collect::<Vec<_>>()
                .join(",")
        },
        |features| features.join(","),
    );
    let mut cmd = Command::new("cargo");
    cmd.current_dir(workspace_root)
        .arg("build")
        .arg("--manifest-path")
        .arg(selected_workspace.join("Cargo.toml"))
        .arg("--package")
        .arg(&board_manifest.package)
        .arg("--lib")
        .arg("--target")
        .arg("x86_64-unknown-none")
        .arg("--target-dir")
        .arg(&target_dir)
        .arg("--no-default-features")
        .arg("--features")
        .arg(&smm_features)
        .arg("--release")
        .arg("--message-format=json-render-diagnostics");
    cmd.env_remove("CARGO_ENCODED_RUSTFLAGS");
    cmd.env(
        "RUSTFLAGS",
        format!("-C panic=abort -C opt-level=s -C relocation-model=pic -C no-redzone=yes -C linker-plugin-lto=no -C embed-bitcode=no -Z function-sections=yes {} {}", crate::toolchain::STAGE_ENV_CHECK_CFG, if selected_features.is_some() { "--cfg fstart_stage_env=\"smm\"" } else { "" }),
    );

    eprintln!(
        "[fstart] building board SMM stage: {}...",
        board_manifest.package
    );
    let output = cmd
        .stderr(std::process::Stdio::inherit())
        .output()
        .map_err(|e| format!("failed to run cargo for SMM stage: {e}"))?;
    if !output.status.success() {
        return Err("board SMM stage build failed".to_string());
    }
    // Link exactly Cargo's current artifact graph, never stale hashed rlibs
    // left in deps/ by another feature or flag selection.
    let deps_dir = target_dir.join("selected-rlibs");
    if deps_dir.exists() {
        fs::remove_dir_all(&deps_dir).map_err(|e| e.to_string())?;
    }
    fs::create_dir_all(&deps_dir).map_err(|e| e.to_string())?;
    for line in output
        .stdout
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
    {
        let artifact: serde_json::Value =
            serde_json::from_slice(line).map_err(|e| e.to_string())?;
        if artifact["reason"] != "compiler-artifact" {
            continue;
        }
        for file in artifact["filenames"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|f| f.as_str())
        {
            let path = Path::new(file);
            if path.starts_with(target_dir.join("x86_64-unknown-none"))
                && path.extension().is_some_and(|ext| ext == "rlib")
            {
                fs::copy(
                    path,
                    deps_dir.join(path.file_name().ok_or("SMM artifact has no name")?),
                )
                .map_err(|e| e.to_string())?;
            }
        }
    }

    Ok(SmmStageBuild {
        deps_dir,
        link_dir: workspace_root
            .join("target")
            .join("smm")
            .join(&board_manifest.board)
            .join("link"),
    })
}

pub fn prepare_selected_board_workspace(
    workspace_root: &Path,
    board_manifest: &crate::board_manifest::BoardManifest,
) -> Result<PathBuf, String> {
    let selected = workspace_root
        .join("target")
        .join("fstart-workspaces")
        .join(&board_manifest.board);
    let board_link = selected.join("boards").join(&board_manifest.rel_dir);
    let board_link_parent = board_link
        .parent()
        .ok_or_else(|| "board workspace link has no parent".to_string())?;
    fs::create_dir_all(board_link_parent)
        .map_err(|e| format!("failed to create selected board workspace: {e}"))?;
    fs::create_dir_all(selected.join("tools"))
        .map_err(|e| format!("failed to create selected tools dir: {e}"))?;

    replace_with_symlink(workspace_root.join("crates"), selected.join("crates"))?;
    replace_with_symlink(
        workspace_root.join("tools").join("fbuild"),
        selected.join("tools").join("fbuild"),
    )?;
    replace_with_file_copy(
        workspace_root.join("Cargo.lock"),
        selected.join("Cargo.lock"),
    )?;
    replace_with_symlink(board_manifest.dir.clone(), board_link)?;

    let rel_dir = board_manifest.rel_dir.to_str().ok_or_else(|| {
        format!(
            "board directory is not valid UTF-8: {}",
            board_manifest.rel_dir.display()
        )
    })?;
    let root_manifest = fs::read_to_string(workspace_root.join("Cargo.toml"))
        .map_err(|e| format!("failed to read root Cargo.toml: {e}"))?;
    let manifest = selected_workspace_manifest(&root_manifest, rel_dir)?;
    fs::write(selected.join("Cargo.toml"), manifest)
        .map_err(|e| format!("failed to write selected board workspace manifest: {e}"))?;
    Ok(selected)
}

/// Disposable all-board lock prototype. Never replaces the source workspace.
pub(crate) fn prepare_inventory_workspace(
    root: &Path,
    boards: &[crate::board_manifest::BoardManifest],
) -> Result<PathBuf, String> {
    let directory = root.join("target/fstart-lock-prototype");
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    for name in ["crates", "tools", "boards"] {
        replace_with_symlink(root.join(name), directory.join(name))?;
    }
    replace_with_file_copy(root.join("Cargo.lock"), directory.join("Cargo.lock"))?;
    let original = fs::read_to_string(root.join("Cargo.toml")).map_err(|e| e.to_string())?;
    let marker = "members = [";
    let at = original.find(marker).ok_or("missing workspace members")? + marker.len();
    let additions = boards
        .iter()
        .map(|board| {
            serde_json::to_string(&format!("boards/{}", board.rel_dir.display()))
                .map(|path| format!("  {path},\n"))
        })
        .collect::<Result<String, _>>()
        .map_err(|e| e.to_string())?;
    let manifest = format!("{}\n{}{}", &original[..at], additions, &original[at..]);
    let manifest = manifest
        .lines()
        .filter(|line| !line.trim_start().starts_with("exclude = "))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(directory.join("Cargo.toml"), manifest).map_err(|e| e.to_string())?;
    Ok(directory)
}

fn replace_with_symlink(target: PathBuf, link: PathBuf) -> Result<(), String> {
    remove_path_if_present(&link)?;
    std::os::unix::fs::symlink(&target, &link).map_err(|e| {
        format!(
            "failed to symlink {} -> {}: {e}",
            link.display(),
            target.display()
        )
    })
}

fn replace_with_file_copy(source: PathBuf, destination: PathBuf) -> Result<(), String> {
    remove_path_if_present(&destination)?;
    fs::copy(&source, &destination).map(|_| ()).map_err(|e| {
        format!(
            "failed to copy {} -> {}: {e}",
            source.display(),
            destination.display()
        )
    })
}

fn remove_path_if_present(path: &Path) -> Result<(), String> {
    if path.exists() || path.is_symlink() {
        fs::remove_file(path)
            .or_else(|_| fs::remove_dir(path))
            .map_err(|e| format!("failed to replace {}: {e}", path.display()))?;
    }
    Ok(())
}

fn selected_workspace_manifest(root_manifest: &str, board_rel_dir: &str) -> Result<String, String> {
    let members_start = root_manifest
        .find("members = [")
        .ok_or_else(|| "root Cargo.toml has no workspace members list".to_string())?;
    let members_end = root_manifest[members_start..]
        .find("]\n")
        .map(|offset| members_start + offset + 2)
        .ok_or_else(|| "root Cargo.toml workspace members list is unterminated".to_string())?;

    let mut manifest = String::new();
    manifest.push_str(&root_manifest[..members_start]);
    manifest.push_str(&format!("members = [\n  \"boards/{board_rel_dir}\",\n]\n"));
    manifest.push_str(&root_manifest[members_end..]);
    manifest = manifest
        .lines()
        .filter(|line| !line.trim_start().starts_with("exclude = "))
        .collect::<Vec<_>>()
        .join("\n");
    manifest.push('\n');
    Ok(manifest)
}

fn max_smm_cpus(stages: &StageLayout) -> Option<u16> {
    match stages {
        StageLayout::Monolithic(stage) => stage.build.mp.filter(|mp| mp.smm).map(|mp| mp.max_cpus),
        StageLayout::MultiStage(stages) => stages
            .iter()
            .filter_map(|stage| stage.build.mp.filter(|mp| mp.smm).map(|mp| mp.max_cpus))
            .max(),
    }
}

#[allow(clippy::too_many_arguments)]
fn build_one_stage(
    workspace_root: &std::path::Path,
    board_manifest: &crate::board_manifest::BoardManifest,
    config: &fstart_core::BoardConfig,
    stage_name: Option<&str>,
    stage_env: &str,
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

    let selected_workspace = prepare_selected_board_workspace(workspace_root, board_manifest)?;

    let mut cmd = Command::new("cargo");
    cmd.current_dir(workspace_root);
    cmd.arg("build");
    cmd.arg("--manifest-path")
        .arg(selected_workspace.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(workspace_root.join("target"));
    cmd.arg("--package").arg(&board_manifest.package);
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
    rustflags.push_str(" --cfg fstart_stage_env=\"");
    rustflags.push_str(stage_env);
    rustflags.push('"');
    rustflags.push(' ');
    rustflags.push_str(crate::toolchain::STAGE_ENV_CHECK_CFG);
    rustflags.push_str(" -Clink-arg=-T");
    rustflags.push_str(&link_ld.display().to_string());
    if let Ok(extra) = std::env::var("FSTART_EXTRA_RUSTFLAGS") {
        rustflags.push(' ');
        rustflags.push_str(&extra);
    }
    cmd.env("RUSTFLAGS", &rustflags);

    cmd.env("FSTART_RUST_BOARD", &board_manifest.board);
    cmd.env("FSTART_LINKER_SCRIPT", &link_ld);
    cmd.env("FSTART_STAGE_FEATURES", features);
    cmd.env("FSTART_STAGE_ENV", stage_env);
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
        board_manifest.package, stage_bin
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

    let elf_path = workspace_root
        .join("target")
        .join(target)
        .join(profile)
        .join(stage_bin);

    let final_elf = if let Some(name) = stage_name {
        let dest = elf_path.with_file_name(format!("fstart-{name}"));
        std::fs::copy(&elf_path, &dest).map_err(|e| format!("failed to copy stage binary: {e}"))?;
        dest
    } else {
        elf_path.clone()
    };

    let run_path = if needs_flat_binary {
        let bin_path = final_elf.with_extension("bin");
        eprintln!(
            "[fstart] elf-to-bin: {} -> {}",
            final_elf.display(),
            bin_path.display()
        );
        write_flat_binary(&final_elf, &bin_path)?;

        if let SocImageFormat::AllwinnerEgon = soc_format {
            fstart_image_build::image::egon::patch_file(&bin_path)?;
        }

        bin_path
    } else {
        final_elf.clone()
    };

    eprintln!("[fstart] built: {}", run_path.display());
    Ok((final_elf, run_path))
}

pub fn workspace_root_pub() -> Result<PathBuf, String> {
    workspace_root()
}

pub(crate) fn write_flat_binary(elf_path: &Path, bin_path: &Path) -> Result<(), String> {
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
