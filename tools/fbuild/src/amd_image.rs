//! Host image-format dispatch of AMD images, not platform identity or firmware initialization.
use crate::compiler_inputs;
use fstart_image_build::{StageBinary, amd::assembly::AmdAssembly};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};

fn read(path: &Path) -> Result<Vec<u8>, String> {
    fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Compose the AMD image from the built `stages` and the assembled `ffs`,
/// if the image has one, into `directory`, with a receipt of its inputs.
pub fn compose(
    root: &Path,
    board: &str,
    directory: &Path,
    operation: &AmdAssembly,
    stages: &[StageBinary],
    ffs: Option<&Path>,
) -> Result<PathBuf, String> {
    let bindings = operation.bindings()?;
    let inputs = compiler_inputs::collect(root, &bindings)?;
    let reset = stages
        .iter()
        .find(|stage| stage.name == operation.reset_unit)
        .ok_or("the reset stage was not built")?;
    let reset_bytes = read(&reset.run_path)?;
    let ffs_bytes = ffs.map(read).transpose()?;
    let sources: Vec<_> = inputs.iter().map(|input| input.bytes()).collect();
    let image = operation.assemble(&sources, &reset_bytes, ffs_bytes.as_deref())?;
    let whole = image.is_whole_flash(operation.flash_size);
    let output = directory.join(if whole {
        format!("{board}.rom")
    } else {
        format!("{board}-psp-fragment.bin")
    });
    let sha256 = |bytes: &[u8]| format!("{:x}", Sha256::digest(bytes));
    fs::write(&output, &image.bytes).map_err(|e| format!("{}: {e}", output.display()))?;
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
    fs::write(
        &metadata,
        serde_json::to_vec_pretty(&receipt).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("{}: {e}", metadata.display()))?;
    if whole {
        eprintln!(
            "[fstart] AMD flash image: {} (reset image at {:#x})",
            output.display(),
            image.facts.reset_offset
        );
    } else {
        eprintln!(
            "[fstart] PSP partition fragment {} at flash offset {:#x}; NOT a factory/flash-ready image",
            output.display(),
            image.offset
        );
    }
    Ok(output)
}
