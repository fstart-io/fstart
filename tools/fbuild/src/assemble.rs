use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Assembled firmware image plus the stage binaries it was built from. The
/// binaries travel with the build result so `run` can attach a stage
/// without guessing build-tree paths.
pub struct AssembledImage {
    /// The final image step's artefact.
    pub image: PathBuf,
    /// Every image step's artefact, by step name.
    pub artifacts: BTreeMap<String, PathBuf>,
    pub stages: Vec<fstart_image_build::StageBinary>,
}

/// The named external inputs a caller bound (`--input NAME=PATH`), keyed
/// by input name. `aliases` are the dedicated flags (`--kernel`,
/// `--secure-firmware`, …) with the input names they bind.
pub fn supplied_inputs(
    aliases: &[(&str, Option<&str>)],
    named: &[(String, String)],
) -> Result<BTreeMap<String, PathBuf>, String> {
    let mut supplied = BTreeMap::new();
    for (name, path) in aliases
        .iter()
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
        artifacts: outputs.artifacts,
        stages,
    })
}
