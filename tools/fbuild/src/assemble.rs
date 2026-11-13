use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Assembled firmware image plus the stage binaries it was built from. The
/// binaries travel with the build result so `run` can place the linked stage
/// (reset vector, anchor) without guessing build-tree paths.
pub struct AssembledImage {
    /// The final image step's artefact.
    pub image: PathBuf,
    /// The FFS step's artefact, when the plan has one: what the current
    /// QEMU runner attaches (it composes its own machine flash from it).
    pub ffs: Option<PathBuf>,
    pub stages: Vec<fstart_image_build::StageBinary>,
}

/// The named external inputs a caller bound, keyed by the plan's input
/// names. The `kernel`/`firmware`/`fit` flags are aliases for those names.
pub fn supplied_inputs(
    kernel_path: Option<&str>,
    firmware_path: Option<&str>,
    fit_path: Option<&str>,
    named: &[(String, String)],
) -> Result<BTreeMap<String, PathBuf>, String> {
    let mut supplied = BTreeMap::new();
    for (name, path) in [
        ("kernel", kernel_path),
        ("firmware", firmware_path),
        ("fit", fit_path),
    ]
    .into_iter()
    .filter_map(|(name, path)| path.map(|path| (name.to_string(), path.to_string())))
    .chain(named.iter().cloned())
    {
        if supplied
            .insert(name.clone(), PathBuf::from(&path))
            .is_some()
        {
            return Err(format!("input {name} was bound twice"));
        }
    }
    Ok(supplied)
}

#[allow(clippy::too_many_arguments)]
pub fn assemble_with_resolved(
    workspace_root: &Path,
    board_manifest: &crate::board_manifest::BoardManifest,
    resolved: &crate::resolved_image::ResolvedImage,
    release: bool,
    supplied: &BTreeMap<String, PathBuf>,
    payload_files: &[(String, String)],
    signing_key: Option<&Path>,
) -> Result<AssembledImage, String> {
    let (stages, outputs) = resolved.assemble(
        workspace_root,
        board_manifest,
        release,
        supplied,
        payload_files,
        signing_key,
    )?;
    Ok(AssembledImage {
        image: outputs.image,
        ffs: outputs.ffs,
        stages,
    })
}
