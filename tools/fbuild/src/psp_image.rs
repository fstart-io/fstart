//! Host image-format dispatch, not platform identity or firmware initialization.
use crate::{board_manifest::BoardManifest, compiler_inputs, resolved_image::ResolvedImage};
use fstart_image_build::amd::assembly::PicassoAssembly;
use sha2::{Digest, Sha256};
use std::{fs, io::Write, path::Path};

pub fn assemble(
    root: &Path,
    board: &BoardManifest,
    plan: &ResolvedImage,
    operation: &PicassoAssembly,
    release: bool,
) -> Result<crate::assemble::AssembledImage, String> {
    let mut inputs = Vec::new();
    let mut total = 0usize;
    for binding in operation.bindings()? {
        let mut collected = compiler_inputs::collect(root, &[binding])?;
        let input = collected.pop().ok_or("missing PSP image input")?;
        total = total
            .checked_add(input.bytes().len())
            .ok_or("PSP image input size overflow")?;
        if total > (operation.files.partition_end - operation.files.efs_offset) as usize + 65536 {
            return Err("PSP image input aggregate exceeds partition budget".into());
        }
        inputs.push(input);
    }
    let build = plan.build(root, board, release)?;
    let reset = build
        .stages
        .iter()
        .find(|stage| stage.name == operation.reset_unit)
        .ok_or("missing reset producer")?;
    let reset_bytes = fs::read(&reset.run_path).map_err(|e| e.to_string())?;
    let sources: Vec<_> = inputs.iter().map(|input| input.bytes()).collect();
    let packed = operation.pack(&reset_bytes, &sources)?;
    let mut hash = Sha256::new();
    hash.update(b"picasso-psp-fragment-v1");
    for input in &inputs {
        input.hash(&mut hash)?;
    }
    hash.update(Sha256::digest(&reset_bytes));
    let directory = plan
        .artifact_dir(root, &board.board, release)?
        .join("image")
        .join(format!("{:x}", hash.finalize()));
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let records = inputs
        .iter()
        .map(|input| input.publish(&directory).map(|(_, record)| record))
        .collect::<Result<Vec<_>, _>>()?;
    let image = directory.join("psp-fragment.bin");
    // No output overwrite/repair. Interrupted publication can leave partial files.
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&image)
        .map_err(|e| e.to_string())?;
    output
        .write_all(&packed.bytes)
        .and_then(|()| output.sync_all())
        .map_err(|e| e.to_string())?;
    let receipt = serde_json::json!({
        "kind": "psp-partition-fragment", "flash_offset": packed.offset,
        "bytes": packed.bytes.len(), "psp_directory": packed.psp_directory, "bios_directory": packed.bios_directory,
        "sha256": format!("{:x}", Sha256::digest(&packed.bytes)), "inputs": records,
        "reset_elf": reset.path, "reset_flat": reset.run_path,
        "reset_sha256": format!("{:x}", Sha256::digest(&reset_bytes)),
    });
    let mut metadata = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join("psp-fragment.json"))
        .map_err(|e| e.to_string())?;
    metadata
        .write_all(&serde_json::to_vec_pretty(&receipt).map_err(|e| e.to_string())?)
        .and_then(|()| metadata.sync_all())
        .map_err(|e| e.to_string())?;
    eprintln!(
        "[fstart] PSP partition fragment {} at flash offset {:#x}; NOT a factory/flash-ready image",
        image.display(),
        packed.offset
    );
    Ok(crate::assemble::AssembledImage {
        image,
        stages: build.stages,
    })
}
