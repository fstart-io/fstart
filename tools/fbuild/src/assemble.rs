//! Assemble exactly the resolved image operation and its declared inputs.
use crate::{board_manifest::BoardManifest, resolved_image::ResolvedImage};
use fstart_image_build::build_plan::Assembly;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

pub struct AssembledImage {
    pub image: PathBuf,
    pub stages: Vec<fstart_image_build::StageBinary>,
}

/// `run`/`flash` use the FFS operation, as before. Only `assemble` performs
/// optional AMD/SP3 vendor-image composition.
#[derive(Clone, Copy)]
pub enum Output {
    Ffs,
    Complete,
}

#[allow(clippy::too_many_arguments)]
pub fn assemble(
    root: &Path,
    board: &BoardManifest,
    resolved: &ResolvedImage,
    release: bool,
    supplied: &BTreeMap<String, PathBuf>,
    payload_files: &[(String, String)],
    signing_key: Option<&Path>,
    output: Output,
) -> Result<AssembledImage, String> {
    let inputs = resolved.build.plan.resolve_inputs(&board.dir, supplied)?;
    let directory = resolved
        .artifact_dir(root, &board.board, release)?
        .join("image");
    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    if matches!(resolved.build.plan.assembly, Assembly::Unassembled { .. }) {
        return Err("plan has no image assembly contract".into());
    }
    let build = resolved.build(root, board, release)?;
    let ffs = match resolved.build.plan.assembly.ffs() {
        Ok(recipe) => Some(fstart_image_build::assemble::assemble(
            root,
            &board.dir,
            &board.board,
            recipe,
            &build.stages,
            Some(&directory),
            &inputs,
            payload_files,
            signing_key,
        )?),
        Err(_) => {
            if !payload_files.is_empty() || signing_key.is_some() {
                return Err("PSP fragment does not consume payload files or signing keys".into());
            }
            None
        }
    };
    let image = match output {
        Output::Ffs => ffs.ok_or("missing FFS output")?,
        Output::Complete => match &resolved.build.plan.assembly {
            Assembly::Amd(image) => crate::amd_image::compose(
                root,
                &board.board,
                &directory,
                image,
                &build.stages,
                ffs.as_deref(),
            )?,
            Assembly::Sp3Flash(image) => compose_sp3_flash(
                root,
                image,
                ffs.as_deref().ok_or("missing SP3 FFS output")?,
                &build.stages,
            )?,
            Assembly::Ffs(_) => ffs.ok_or("missing FFS output")?,
            Assembly::Unassembled { .. } => unreachable!(),
        },
    };
    Ok(AssembledImage {
        image,
        stages: build.stages,
    })
}

/// Keep the upstream SP3 compositor and its vendor-bank/mirror semantics.
/// Only the orchestration input changes: the explicit plan and built artifacts.
fn compose_sp3_flash(
    workspace_root: &Path,
    image: &fstart_image_build::sp3_flash::Sp3FlashAssembly,
    ffs_path: &Path,
    stages: &[fstart_image_build::StageBinary],
) -> Result<PathBuf, String> {
    use fstart_image_build::sp3_flash::{HALF_SIZE, HalfAction, name};
    let read = |path: &Path| std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()));
    let plan = &image.compose;
    let vendor_path = workspace_root.join(&image.vendor_image);
    let vendor = if image.packed.is_empty() {
        eprintln!("[fstart] SP3 vendor ROM {}", vendor_path.display());
        read(&vendor_path)?
    } else {
        let read = |path: &str| read(Path::new(path));
        let rom = fstart_image_build::sp3_flash::packed_rom(&image.packed, plan, &read)?;
        for half in &image.packed {
            eprintln!(
                "[fstart] SP3 {} PSP firmware packed from its files",
                name(half.generation)
            );
        }
        rom
    };
    let ffs = read(ffs_path)?;
    let reset = stages
        .iter()
        .find(|stage| stage.name == image.reset_unit)
        .ok_or("SP3 reset unit was not built")?;
    let flash =
        fstart_image_build::sp3_flash::compose(&vendor, plan, &read(&reset.run_path)?, &ffs)?;
    if !flash.patched.locator || !flash.patched.flash {
        eprintln!(
            "[fstart] warning: the SP3 reset unit links no FFS locator or flash anchor; it cannot read its FFS"
        );
    }
    let survey = &flash.survey;
    for (half, action) in &survey.halves {
        match action {
            HalfAction::Compose(layout) => eprintln!(
                "[fstart] SP3 {half:?} half ({}): EFS and directories to {:#x}",
                name(layout.generation),
                layout.prefix_end
            ),
            HalfAction::Keep(generation) => eprintln!(
                "[fstart] warning: SP3 {half:?} half kept from {} ({}: not supported by the board)",
                vendor_path.display(),
                generation.map_or("no EFS", name)
            ),
        }
    }
    let reset_copies = if survey.composed().count() == 1 || flash.image.len() == HALF_SIZE {
        "one composed half"
    } else {
        "each half"
    };
    let out = ffs_path.with_extension("rom");
    std::fs::write(&out, &flash.image).map_err(|e| format!("{}: {e}", out.display()))?;
    if flash.image.len() != plan.boot_window(&flash.image)?.len() {
        let bank = out.with_extension("bank.rom");
        std::fs::write(&bank, &flash.image[..HALF_SIZE])
            .map_err(|e| format!("{}: {e}", bank.display()))?;
        eprintln!(
            "[fstart] SP3 boot bank mirrored at {:x?}; 16 MiB bank alone: {}",
            plan.mirrors,
            bank.display()
        );
    }
    eprintln!(
        "[fstart] SP3 image: {} ({} MiB); FFS {:#x} of {:#x} bytes at {:#x}..{:#x}; reset image {:#x} bytes at {:#x} ({reset_copies})",
        out.display(),
        flash.image.len() >> 20,
        ffs.len(),
        survey.ffs_capacity(),
        survey.ffs.start,
        survey.ffs.end,
        plan.reset.size,
        plan.reset.flash_offset
    );
    Ok(out)
}
