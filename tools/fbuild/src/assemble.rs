use std::path::{Path, PathBuf};

/// Assembled firmware image plus the stage binaries it was built from. The
/// binaries travel with the build result so `run` can place the linked stage
/// (reset vector, anchor) without guessing build-tree paths.
pub struct AssembledImage {
    pub image: PathBuf,
    pub stages: Vec<fstart_image_build::StageBinary>,
}

pub fn assemble_with_parsed(
    workspace_root: &Path,
    board_manifest: crate::board_manifest::BoardManifest,
    parsed: crate::build_plan::ParsedBoard,
    release: bool,
    kernel_path: Option<&str>,
    firmware_path: Option<&str>,
    fit_path: Option<&str>,
) -> Result<AssembledImage, String> {
    let output_dir = if let Some(resolved) = &parsed.resolved {
        resolved.validate_inputs(&board_manifest.dir, kernel_path, firmware_path, fit_path)?;
        Some(
            resolved
                .artifact_dir(workspace_root, &board_manifest.board, release)?
                .join("image"),
        )
    } else {
        None
    };
    let build_result =
        crate::build_board::build_with_parsed(workspace_root, &board_manifest, &parsed, release)?;
    let image = fstart_image_build::assemble::assemble(
        workspace_root,
        &board_manifest.dir,
        &parsed.config,
        &build_result.stages,
        output_dir.as_deref(),
        kernel_path,
        firmware_path,
        fit_path,
        parsed
            .resolved
            .as_ref()
            .and_then(|plan| plan.bootstrap_bindings()),
    )?;
    Ok(AssembledImage {
        image,
        stages: build_result.stages,
    })
}
