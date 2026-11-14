//! Execute a plan's image steps: the one dispatch on image formats.
//!
//! A tool executing a [`BuildPlan`] builds the compilation units, then
//! hands the stage binaries and the caller's external inputs to
//! [`execute`]; it never matches on [`ImageOperation`] itself. Adding a
//! format means adding its operation and its arm here, next to the
//! format's own code — not teaching every tool a new special case.
use crate::build_plan::{BuildPlan, FfsAssembly, FileBinding, ImageOperation};
use crate::{StageBinary, amd::halved_flash};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

/// Everything an image step may consume besides the plan itself.
pub struct ImageContext<'a> {
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

fn read(path: &Path) -> Result<Vec<u8>, String> {
    fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
}

/// A declared file input, read with its size bound enforced, resolved
/// against the workspace root unless absolute.
fn read_binding(root: &Path, binding: &FileBinding) -> Result<Vec<u8>, String> {
    let source = root
        .join(&binding.path)
        .canonicalize()
        .map_err(|e| format!("{}: {e}", binding.path))?;
    let metadata = fs::metadata(&source).map_err(|e| format!("{}: {e}", source.display()))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > binding.capacity {
        return Err(format!(
            "{}: image input is not a nonempty bounded regular file",
            source.display()
        ));
    }
    let bytes = read(&source)?;
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
    if plan.images.is_empty() {
        return Err("plan has no image steps".into());
    }
    fs::create_dir_all(ctx.output_dir).map_err(|e| e.to_string())?;
    let mut outputs = ImageOutputs {
        artifacts: BTreeMap::new(),
        image: ctx.output_dir.to_path_buf(),
        notes: Vec::new(),
    };
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
                crate::assemble::assemble(
                    ctx.workspace_root,
                    ctx.board_dir,
                    &config,
                    ctx.stages,
                    Some(ctx.output_dir),
                    input(FfsAssembly::KERNEL)?.as_deref(),
                    input(FfsAssembly::FIRMWARE)?.as_deref(),
                    input(FfsAssembly::FIT)?.as_deref(),
                    ctx.payload_files,
                    Some(&image.bootstrap),
                    image.boot_image_format,
                    ctx.signing_key,
                )?
            }
            ImageOperation::AmdEfs(operation) => {
                amd_efs(ctx, operation, consumed.as_deref(), &mut outputs.notes)?
            }
            ImageOperation::AmdHalvedFlash(operation) => {
                amd_halved_flash(ctx, operation, consumed.as_deref(), &mut outputs.notes)?
            }
            ImageOperation::X86Flash(operation) => {
                x86_flash(ctx, operation, consumed.as_deref(), &mut outputs.notes)?
            }
        };
        outputs.image = artifact.clone();
        outputs.artifacts.insert(step.name.clone(), artifact);
    }
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
    let (image, composed) =
        operation.compose(&read(&ffs_path)?, &read(&stage.path)?, &stage.path)?;
    notes.extend(composed);
    let out = ffs_path.with_extension("rom");
    write(&out, &image)?;
    notes.push(format!(
        "x86 flash image: {} ({} MiB, FFS at {:#x})",
        out.display(),
        image.len() >> 20,
        operation.ffs_offset
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
    notes: &mut Vec<String>,
) -> Result<PathBuf, String> {
    let bindings = operation.bindings()?;
    let sources = bindings
        .iter()
        .map(|binding| read_binding(ctx.workspace_root, binding))
        .collect::<Result<Vec<_>, _>>()?;
    let reset = stage(ctx.stages, &operation.stage_unit)?;
    let reset_bytes = read(&reset.run_path)?;
    let ffs_bytes = match &operation.ffs {
        Some(_) => Some(read(&ffs_artifact(ffs, "AMD EFS")?)?),
        None => None,
    };
    let source_refs: Vec<_> = sources.iter().map(Vec::as_slice).collect();
    let image = operation.assemble(&source_refs, &reset_bytes, ffs_bytes.as_deref())?;
    let whole = image.is_whole_flash(operation.flash_size);
    let output = ctx.output_dir.join(if whole {
        format!("{}.rom", ctx.board_name)
    } else {
        format!("{}-psp-fragment.bin", ctx.board_name)
    });
    write(&output, &image.bytes)?;
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
    write(
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

/// Compose the consumed FFS artefact and the reset stage into the
/// halved vendor ROM (or its packed halves). Writes `<ffs>.rom`, and
/// `<ffs>.bank.rom` when the readout carries mirrored boot banks.
fn amd_halved_flash(
    ctx: &ImageContext<'_>,
    image: &halved_flash::AmdHalvedFlashAssembly,
    ffs: Option<&Path>,
    notes: &mut Vec<String>,
) -> Result<PathBuf, String> {
    use halved_flash::{HALF_SIZE, HalfAction, name};
    let plan = &image.compose;
    // Relative vendor paths are workspace-relative (`--image-config`
    // replaces the board's vendor ROM).
    let vendor_path = ctx.workspace_root.join(&image.vendor_image);
    let vendor = if image.packed.is_empty() {
        notes.push(format!("AMD vendor ROM {}", vendor_path.display()));
        read(&vendor_path)?
    } else {
        let read = |path: &str| read(Path::new(path));
        let rom = halved_flash::packed_rom(&image.packed, plan, &read)?;
        for half in &image.packed {
            notes.push(format!(
                "AMD {} PSP firmware packed from its files",
                name(half.generation)
            ));
        }
        rom
    };
    let ffs_path = ffs_artifact(ffs, "AMD halved flash")?;
    let ffs = read(&ffs_path)?;
    let reset = stage(ctx.stages, &image.stage_unit)?;
    let flash = halved_flash::compose(&vendor, plan, &read(&reset.run_path)?, &ffs)?;
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
                "warning: AMD {half:?} half kept from {} ({}: not supported by the board)",
                vendor_path.display(),
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
    write(&out, &flash.image)?;
    if flash.image.len() != plan.boot_window(&flash.image)?.len() {
        // Programmers that mirror the bank themselves (a BMC BIOS update)
        // take the 16 MiB boot bank, not the full readout.
        let bank = out.with_extension("bank.rom");
        write(&bank, &flash.image[..HALF_SIZE])?;
        notes.push(format!(
            "AMD boot bank mirrored at {:x?}; 16 MiB bank alone: {}",
            plan.mirrors,
            bank.display()
        ));
    }
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
