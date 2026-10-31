//! Typed board operations. CLI parsing belongs exclusively to main.rs.
use crate::{
    board_manifest::BoardManifest,
    payload::{BuildArgs, PayloadChoice},
    resolved_image::ResolvedImage,
};
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone, clap::Args)]
pub struct ImageArgs {
    #[arg(short, long)]
    pub kernel: Option<String>,
    #[arg(short, long)]
    pub firmware: Option<String>,
    #[arg(long)]
    pub fit: Option<String>,
    /// Named assembly input, e.g. dtb=board.dtb.
    #[arg(long = "input", value_parser = parse_input)]
    pub inputs: Vec<(String, String)>,
}
pub fn parse_input(value: &str) -> Result<(String, String), String> {
    let (name, path) = value.split_once('=').ok_or("input must use NAME=PATH")?;
    if name.is_empty() || path.is_empty() {
        return Err("input must use NAME=PATH".into());
    }
    Ok((name.into(), path.into()))
}
impl ImageArgs {
    pub fn supplied(&self) -> Result<std::collections::BTreeMap<String, PathBuf>, String> {
        let mut supplied = std::collections::BTreeMap::new();
        for (name, path) in [
            ("kernel", &self.kernel),
            ("firmware", &self.firmware),
            ("fit", &self.fit),
        ] {
            if let Some(path) = path {
                supplied.insert(name.into(), PathBuf::from(path));
            }
        }
        for (name, path) in &self.inputs {
            if supplied.insert(name.clone(), path.into()).is_some() {
                return Err(format!("duplicate input {name}"));
            }
        }
        Ok(supplied)
    }
}

pub fn build(
    manifest: &BoardManifest,
    release: bool,
    payload: Option<PayloadChoice>,
    options: &BuildArgs,
) -> Result<crate::build_board::BuildResult, String> {
    let root = crate::build_board::workspace_root_pub()?;
    ResolvedImage::load(&root, manifest, options.selection(payload))?
        .build(&root, manifest, release)
}

pub fn assemble(
    manifest: &BoardManifest,
    release: bool,
    payload: Option<PayloadChoice>,
    image: &ImageArgs,
    options: &BuildArgs,
) -> Result<PathBuf, String> {
    let root = crate::build_board::workspace_root_pub()?;
    let resolved = ResolvedImage::load(&root, manifest, options.selection(payload))?;
    assemble_loaded(
        &root,
        manifest,
        &resolved,
        release,
        image,
        options,
        crate::assemble::Output::Complete,
    )
    .map(|assembled| assembled.image)
}

#[allow(clippy::too_many_arguments)]
fn assemble_loaded(
    root: &Path,
    manifest: &BoardManifest,
    resolved: &ResolvedImage,
    release: bool,
    image: &ImageArgs,
    options: &BuildArgs,
    output: crate::assemble::Output,
) -> Result<crate::assemble::AssembledImage, String> {
    crate::assemble::assemble(
        root,
        manifest,
        resolved,
        release,
        &image.supplied()?,
        &options.payload_files,
        options.signing_key.as_deref(),
        output,
    )
}

#[derive(Debug, Default, Clone, clap::Args)]
pub struct RunArgs {
    #[command(flatten)]
    pub image: ImageArgs,
    #[arg(short, long)]
    pub disk: Option<String>,
    #[arg(short, long)]
    pub memory: Option<String>,
    /// Complete secure pflash image used only by QEMU SBSA-ref.
    #[arg(long)]
    pub secure_firmware: Option<String>,
}

pub fn run(
    manifest: &BoardManifest,
    release: bool,
    payload: Option<PayloadChoice>,
    run: &RunArgs,
    options: &BuildArgs,
) -> Result<(), String> {
    let root = crate::build_board::workspace_root_pub()?;
    let resolved = ResolvedImage::load(&root, manifest, options.selection(payload))?;
    let recipe = resolved.build.plan.assembly.ffs()?;
    let policy = recipe.build.clone();
    let platform = recipe.platform;
    let multi_stage = matches!(recipe.stages, fstart_core::StageLayout::MultiStage(_));
    let needs_firmware_image = policy.qemu_machine.is_some()
        || matches!(&recipe.stages, fstart_core::StageLayout::Monolithic(stage) if stage.build.firmware_image.is_some() || stage.build.verify_firmware);
    let has_payload = !run.image.supplied()?.is_empty()
        || recipe.payload.as_ref().is_some_and(|payload| {
            payload.firmware.is_some()
                || payload.kernel_file.is_some()
                || payload.fit_file.is_some()
                || payload.kind == fstart_core::PayloadKind::FitImage
        });
    if multi_stage || platform.is_x86() || needs_firmware_image || has_payload {
        let assembled = assemble_loaded(
            &root,
            manifest,
            &resolved,
            release,
            &run.image,
            options,
            crate::assemble::Output::Ffs,
        )?;
        let mut sources = vec![("firmware.ffs", assembled.image.as_path())];
        if platform.is_x86() {
            sources.push(("stage.elf", assembled.stages[0].path.as_path()));
        }
        let session = resolved.into_launch(&root, &sources)?;
        let elf = platform.is_x86().then(|| session.path().join("stage.elf"));
        crate::qemu::run(
            &policy,
            platform,
            &session.path().join("firmware.ffs"),
            run.disk.as_deref(),
            run.memory.as_deref(),
            run.secure_firmware.as_deref(),
            elf.as_deref(),
        )
    } else {
        let build = resolved.build(&root, manifest, release)?;
        let session =
            resolved.into_launch(&root, &[("stage.bin", &build.primary_binary().run_path)])?;
        crate::qemu::run(
            &policy,
            platform,
            &session.path().join("stage.bin"),
            run.disk.as_deref(),
            run.memory.as_deref(),
            run.secure_firmware.as_deref(),
            None,
        )
    }
}

#[derive(Debug, Default, Clone, clap::Args)]
pub struct FlashArgs {
    #[arg(long, default_value_t = false)]
    pub probe_run: bool,
    #[arg(long)]
    pub chip: Option<String>,
    #[arg(long)]
    pub probe: Option<String>,
    #[arg(long)]
    pub base_address: Option<String>,
    /// Explicit probe-rs executable; otherwise use PATH.
    #[arg(long)]
    pub probe_rs: Option<PathBuf>,
}

pub fn flash(
    manifest: &BoardManifest,
    release: bool,
    options: &BuildArgs,
    flash: &FlashArgs,
) -> Result<(), String> {
    let root = crate::build_board::workspace_root_pub()?;
    let resolved = ResolvedImage::load(&root, manifest, options.selection(None))?;
    resolved.build.plan.assembly.ffs()?;
    let probe_rs = flash
        .probe_rs
        .clone()
        .map(Ok)
        .unwrap_or_else(find_probe_rs)?;
    let mk_cmd = |subcommand: &str| {
        let mut command = std::process::Command::new(&probe_rs);
        command
            .arg(subcommand)
            .arg("--chip")
            .arg(flash.chip.as_deref().unwrap_or("auto"))
            .args(["--protocol", "jtag"]);
        if let Some(probe) = &flash.probe {
            command.arg("--probe").arg(probe);
        }
        command
    };
    let (elf, image) = if flash.probe_run {
        let elf = resolved
            .build(&root, manifest, release)?
            .primary_binary()
            .path
            .clone();
        (elf, None)
    } else {
        flash.base_address.as_deref().ok_or(
            "flash requires --base-address because fbuild has no board flash programmer defaults",
        )?;
        let assembled = assemble_loaded(
            &root,
            manifest,
            &resolved,
            release,
            &ImageArgs::default(),
            options,
            crate::assemble::Output::Ffs,
        )?;
        (assembled.stages[0].path.clone(), Some(assembled.image))
    };
    let mut sources = vec![("stage.elf", elf.as_path())];
    if let Some(image) = &image {
        sources.push(("firmware.bin", image.as_path()));
    }
    let session = resolved.into_launch(&root, &sources)?;
    if image.is_some() {
        let status = mk_cmd("download")
            .args([
                "--binary-format",
                "bin",
                "--base-address",
                flash.base_address.as_deref().unwrap(),
            ])
            .arg(session.path().join("firmware.bin"))
            .status()
            .map_err(|e| e.to_string())?;
        if !status.success() {
            return Err(format!("probe-rs download failed with {status}"));
        }
    }
    let status = mk_cmd("run")
        .arg(session.path().join("stage.elf"))
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!("probe-rs run failed with {status}"));
    }
    Ok(())
}

fn find_probe_rs() -> Result<PathBuf, String> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|dir| dir.join("probe-rs"))
        .find(|path| path.is_file())
        .ok_or_else(|| "probe-rs not found in PATH".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn duplicate_named_and_legacy_inputs_are_rejected() {
        let image = ImageArgs {
            kernel: Some("kernel".into()),
            inputs: vec![("kernel".into(), "other".into())],
            ..Default::default()
        };
        assert!(image.supplied().unwrap_err().contains("duplicate"));
    }
}
