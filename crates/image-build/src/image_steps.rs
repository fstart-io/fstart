//! Execute a plan's image steps: the one dispatch on image formats.
//!
//! A tool executing a [`BuildPlan`] builds the compilation units, then
//! hands the stage binaries and the caller's external inputs to
//! [`execute`]; it never matches on [`ImageOperation`] itself. Adding a
//! format means adding its operation and its arm here, next to the
//! format's own code — not teaching every tool a new special case.
use crate::build_plan::{BuildPlan, FfsAssembly, FileBinding, ImageOperation};
use crate::files::Files;
use crate::{StageBinary, amd::halved_flash};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// Everything an image step may consume besides the plan itself.
pub struct ImageContext<'a> {
    /// Where the steps read their inputs and write their artefacts: the
    /// disk, or an in-memory tree for a bundle ([`crate::bundle`]).
    pub files: &'a dyn Files,
    pub workspace_root: &'a Path,
    pub board_dir: &'a Path,
    pub board_name: &'a str,
    pub stages: &'a [StageBinary],
    pub output_dir: &'a Path,
    /// External files bound by name (`--input NAME=PATH`), as declared by
    /// the plan's `inputs`.
    pub inputs: &'a BTreeMap<String, PathBuf>,
    /// Named files for a coreboot payload (`--payload-file NAME=PATH`).
    pub payload_files: &'a [(String, String)],
    pub signing_key: Option<&'a Path>,
    /// What the image's settings store starts with (`--setting KEY=VALUE`,
    /// Secure Boot), checked against the plan's options
    /// ([`crate::options::Options::seed`]).
    pub settings: &'a crate::options::Settings,
    /// Optional override for the signed FFS `Root` firmware version.
    pub fw_version: Option<u32>,
    /// Optional override for the signed FFS `Root` rollback security version.
    pub security_version: Option<u64>,
}

/// The artefacts of the executed steps, by step name, with the final
/// image and human-readable notes the steps produced.
#[derive(Debug)]
pub struct ImageOutputs {
    pub artifacts: BTreeMap<String, PathBuf>,
    /// The last step's artefact: the image `assemble` reports.
    pub image: PathBuf,
    pub notes: Vec<String>,
}

/// A declared file input, read with its size bound enforced: a blob, or
/// a path resolved against the workspace root unless absolute
/// ([`Files::read_named`]).
fn read_binding(files: &dyn Files, root: &Path, binding: &FileBinding) -> Result<Vec<u8>, String> {
    let source = Path::new(&binding.path);
    let size = files.named_len(root, source)?;
    if size == 0 || size > binding.capacity {
        return Err(format!(
            "{}: image input is not a nonempty bounded regular file",
            source.display()
        ));
    }
    let bytes = files.read_named(root, source)?;
    if bytes.is_empty() || bytes.len() as u64 > binding.capacity {
        return Err(format!(
            "{}: image input changed outside its size bound",
            source.display()
        ));
    }
    Ok(bytes)
}

fn sha256(bytes: &[u8]) -> String {
    fstart_crypto::digest::hash_sha256(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn stage<'a>(stages: &'a [StageBinary], unit: &str) -> Result<&'a StageBinary, String> {
    stages
        .iter()
        .find(|stage| stage.name == unit)
        .ok_or_else(|| format!("stage unit {unit} was not built"))
}

/// Run every image step of `plan` in order. A plan with no steps has no
/// image contract and is an error here, not an empty success.
pub fn execute(plan: &BuildPlan, ctx: &ImageContext<'_>) -> Result<ImageOutputs, String> {
    plan.order()?;
    // A halved flash packs the generations whose blobs are there.
    let mut plan = plan.clone();
    halved_flash::keep_present(&mut plan, &|path| {
        ctx.files.named_exists(ctx.workspace_root, Path::new(path))
    })?;
    let plan = &plan;
    if plan.images.is_empty() {
        return Err("plan has no image steps".into());
    }
    let mut seed = plan.options.seed(ctx.settings)?;
    ctx.files.create_dir_all(ctx.output_dir)?;
    let mut outputs = ImageOutputs {
        artifacts: BTreeMap::new(),
        image: ctx.output_dir.to_path_buf(),
        notes: Vec::new(),
    };
    let mut ffs_trusts: BTreeMap<String, [u8; fstart_core::ffs::trust::TRUST_SIZE]> =
        BTreeMap::new();
    let input = |name: &str| -> Result<Option<String>, String> {
        ctx.inputs
            .get(name)
            .map(|path| {
                path.to_str()
                    .map(str::to_owned)
                    .ok_or_else(|| format!("input {name} path is not UTF-8"))
            })
            .transpose()
    };
    for step in &plan.images {
        // order() checked that a consumed step comes earlier.
        let consumed = step
            .operation
            .consumes()
            .map(|name| {
                outputs
                    .artifacts
                    .get(name)
                    .cloned()
                    .ok_or_else(|| format!("image step {name} produced no artefact"))
            })
            .transpose()?;
        let artifact = match &step.operation {
            ImageOperation::Ffs(image) => {
                let config = image.config(ctx.board_name)?;
                let root_opts = crate::assemble::FfsRootOptions {
                    fw_version: ctx.fw_version.unwrap_or(image.fw_version),
                    security_version: ctx.security_version.unwrap_or(image.security_version),
                    rw_slot: image.rw_slot,
                    omit_stages: &image.omit_stages,
                };
                let (path, trust_bytes) = crate::assemble::assemble_with_trust(
                    ctx.files,
                    ctx.workspace_root,
                    ctx.board_dir,
                    &config,
                    ctx.stages,
                    Some(ctx.output_dir),
                    input(FfsAssembly::KERNEL)?.as_deref(),
                    input(FfsAssembly::FIRMWARE)?.as_deref(),
                    input(FfsAssembly::FIT)?.as_deref(),
                    input(FfsAssembly::DTB)?.as_deref(),
                    ctx.payload_files,
                    Some(&image.bootstrap),
                    image.boot_image_format,
                    ctx.signing_key,
                    &root_opts,
                )?;
                ffs_trusts.insert(step.name.clone(), trust_bytes);
                path
            }
            ImageOperation::AmdEfs(operation) => amd_efs(
                ctx,
                operation,
                consumed.as_deref(),
                seed.take().as_ref(),
                &mut outputs.notes,
            )?,
            ImageOperation::AmdHalvedFlash(operation) => amd_halved_flash(
                ctx,
                operation,
                consumed.as_deref(),
                seed.take(),
                &mut outputs.notes,
            )?,
            ImageOperation::X86Flash(operation) => {
                x86_flash(ctx, operation, consumed.as_deref(), &mut outputs.notes)?
            }
            ImageOperation::X86AbFlash(operation) => {
                let trust_bytes = ffs_trusts.get(&operation.ffs).ok_or_else(|| {
                    format!(
                        "x86 A/B flash step '{}' did not receive trust policy from FFS step '{}'",
                        step.name, operation.ffs
                    )
                })?;
                x86_ab_flash(
                    ctx,
                    operation,
                    consumed.as_deref(),
                    trust_bytes,
                    seed.take(),
                    &mut outputs.notes,
                )?
            }
        };
        outputs.image = artifact.clone();
        outputs.artifacts.insert(step.name.clone(), artifact);
    }
    if seed.is_some() {
        return Err("settings: no image step holds the settings store".into());
    }
    outputs.notes.extend(ctx.settings.notes());
    Ok(outputs)
}

fn ffs_artifact(ffs: Option<&Path>, format: &str) -> Result<PathBuf, String> {
    ffs.map(Path::to_path_buf)
        .ok_or_else(|| format!("{format} step names no FFS step"))
}

/// Compose the consumed FFS artefact and the linked stage into an x86
/// flash image, written next to the FFS as `<ffs>.rom`.
fn x86_flash(
    ctx: &ImageContext<'_>,
    operation: &crate::x86_flash::X86FlashAssembly,
    ffs: Option<&Path>,
    notes: &mut Vec<String>,
) -> Result<PathBuf, String> {
    let ffs_path = ffs_artifact(ffs, "x86 flash")?;
    let stage = stage(ctx.stages, &operation.stage_unit)?;
    let (image, composed) = operation.compose(
        &ctx.files.read(&ffs_path)?,
        &ctx.files.read(&stage.path)?,
        &stage.path,
    )?;
    notes.extend(composed);
    let out = ffs_path.with_extension("rom");
    ctx.files.write(&out, &image)?;
    notes.push(format!(
        "x86 flash image: {} ({} MiB, FFS at {:#x})",
        out.display(),
        image.len() >> 20,
        operation.ffs_offset
    ));
    Ok(out)
}

/// Compose the consumed RW-slot FFS artefact and the linked RO stage into
/// an x86 RO/A(/B) flash image, written next to the FFS as `<ffs>.rom`.
fn x86_ab_flash(
    ctx: &ImageContext<'_>,
    operation: &crate::x86_flash::X86AbFlashAssembly,
    ffs: Option<&Path>,
    trust_bytes: &[u8; fstart_core::ffs::trust::TRUST_SIZE],
    seed: Option<crate::options::Seed>,
    notes: &mut Vec<String>,
) -> Result<PathBuf, String> {
    let ffs_path = ffs_artifact(ffs, "x86 A/B flash")?;
    if let Some(seed) = &seed {
        let seed_len = u32::try_from(seed.bytes.len())
            .map_err(|_| "settings: seed length exceeds u32".to_string())?;
        if operation.nvram != Some((seed.offset, seed_len)) {
            return Err(format!(
                "settings: the options' store at {:#x} ({:#x} bytes) does not match x86 A/B NVRAM {:x?}",
                seed.offset,
                seed.bytes.len(),
                operation.nvram
            ));
        }
    }
    let ro = stage(ctx.stages, &operation.ro_stage_unit)?;
    let (image, composed) = operation.compose(
        &ctx.files.read(&ffs_path)?,
        trust_bytes,
        &ctx.files.read(&ro.path)?,
        &ro.path,
        seed.as_ref().map(|s| s.bytes.as_slice()),
    )?;
    notes.extend(composed);
    let out = ffs_path.with_extension("rom");
    ctx.files.write(&out, &image)?;
    notes.push(format!(
        "x86 A/B flash image: {} ({} MiB)",
        out.display(),
        image.len() >> 20,
    ));
    Ok(out)
}

/// Compose an AMD EFS image from its PSP firmware sources, the reset
/// stage and the consumed FFS artefact if the image has a window for
/// it, with a receipt of its inputs.
fn amd_efs(
    ctx: &ImageContext<'_>,
    operation: &crate::amd::assembly::AmdAssembly,
    ffs: Option<&Path>,
    seed: Option<&crate::options::Seed>,
    notes: &mut Vec<String>,
) -> Result<PathBuf, String> {
    let bindings = operation.bindings()?;
    let sources = bindings
        .iter()
        .map(|binding| read_binding(ctx.files, ctx.workspace_root, binding))
        .collect::<Result<Vec<_>, _>>()?;
    let reset = stage(ctx.stages, &operation.stage_unit)?;
    let reset_bytes = ctx.files.read(&reset.run_path)?;
    let ffs_bytes = match &operation.ffs {
        Some(_) => Some(ctx.files.read(&ffs_artifact(ffs, "AMD EFS")?)?),
        None => None,
    };
    let source_refs: Vec<_> = sources.iter().map(Vec::as_slice).collect();
    let image =
        operation.assemble_seeded(&source_refs, &reset_bytes, ffs_bytes.as_deref(), seed)?;
    let whole = image.is_whole_flash(operation.flash_size);
    let output = ctx.output_dir.join(if whole {
        format!("{}.rom", ctx.board_name)
    } else {
        format!("{}-psp-fragment.bin", ctx.board_name)
    });
    ctx.files.write(&output, &image.bytes)?;
    let receipt = serde_json::json!({
        "flash_offset": image.offset,
        "bytes": image.bytes.len(),
        "whole_flash": whole,
        "sha256": sha256(&image.bytes),
        "efs": image.facts.efs,
        "reset_offset": image.facts.reset_offset,
        "inputs": bindings.iter().zip(&sources).map(|(binding, bytes)| serde_json::json!({
            "environment": binding.environment, "path": binding.path, "sha256": sha256(bytes),
        })).collect::<Vec<_>>(),
        "reset_flat": reset.run_path,
        "reset_sha256": sha256(&reset_bytes),
        "ffs_sha256": ffs_bytes.as_deref().map(sha256),
    });
    let metadata = output.with_extension("json");
    ctx.files.write(
        &metadata,
        &serde_json::to_vec_pretty(&receipt).map_err(|e| e.to_string())?,
    )?;
    notes.push(if whole {
        format!(
            "AMD flash image: {} (reset image at {:#x})",
            output.display(),
            image.facts.reset_offset
        )
    } else {
        format!(
            "PSP partition fragment {} at flash offset {:#x}; NOT a factory/flash-ready image",
            output.display(),
            image.offset
        )
    });
    Ok(output)
}

/// Compose the consumed FFS artefact and the reset stage with the halves
/// packed from their files. Writes `<ffs>.rom`.
fn amd_halved_flash(
    ctx: &ImageContext<'_>,
    image: &halved_flash::AmdHalvedFlashAssembly,
    ffs: Option<&Path>,
    seed: Option<crate::options::Seed>,
    notes: &mut Vec<String>,
) -> Result<PathBuf, String> {
    use halved_flash::{HALF_SIZE, HalfAction, name};
    let plan = &image.compose;
    // The options' store is the flash plan's: seeded where the boot reads.
    if let Some(seed) = &seed
        && plan
            .nvram
            .map(|slot| (slot.flash_offset, slot.size as usize))
            != Some((seed.offset, seed.bytes.len()))
    {
        return Err(format!(
            "settings: the options' store at {:#x} is not the flash plan's {:x?}",
            seed.offset, plan.nvram
        ));
    }
    let rom = halved_flash::packed_rom(&image.packed, plan, &|path: &str| {
        ctx.files.read_named(ctx.workspace_root, Path::new(path))
    })?;
    for half in &image.packed {
        notes.push(format!(
            "AMD {} PSP firmware packed from its files",
            name(half.generation)
        ));
    }
    let ffs_path = ffs_artifact(ffs, "AMD halved flash")?;
    let ffs = ctx.files.read(&ffs_path)?;
    let reset = stage(ctx.stages, &image.stage_unit)?;
    let flash = halved_flash::compose_seeded(
        &rom,
        plan,
        &ctx.files.read(&reset.run_path)?,
        &ffs,
        seed.as_ref().map(|seed| seed.bytes.as_slice()),
    )?;
    if !flash.patched.locator || !flash.patched.flash {
        notes.push(
            "warning: the AMD reset stage unit links no FFS locator or flash anchor; \
             it cannot read its FFS"
                .into(),
        );
    }
    let survey = &flash.survey;
    for (half, action) in &survey.halves {
        match action {
            HalfAction::Compose(layout) => notes.push(format!(
                "AMD {half:?} half ({}): EFS and directories to {:#x}",
                name(layout.generation),
                layout.prefix_end,
            )),
            HalfAction::Keep(generation) => notes.push(format!(
                "warning: AMD {half:?} half kept ({}: not supported by the board)",
                generation.map_or("no EFS", name),
            )),
        }
    }
    let reset_copies = if survey.composed().count() == 1 || flash.image.len() == HALF_SIZE {
        "one composed half"
    } else {
        "each half"
    };
    let out = ffs_path.with_extension("rom");
    ctx.files.write(&out, &flash.image)?;
    notes.push(format!(
        "AMD halved flash image: {} ({} MiB); FFS {:#x} of {:#x} bytes at {:#x}..{:#x}; \
         reset image {:#x} bytes at {:#x} ({reset_copies})",
        out.display(),
        flash.image.len() >> 20,
        ffs.len(),
        survey.ffs_capacity(),
        survey.ffs.start,
        survey.ffs.end,
        plan.reset.size,
        plan.reset.flash_offset,
    ));
    Ok(out)
}
