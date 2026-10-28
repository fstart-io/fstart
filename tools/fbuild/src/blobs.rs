//! `fbuild blobs`: fill a blob directory from a vendor flash image with the
//! files the board's and its SoC's AMD firmware tables name, at the paths
//! `--blobs` builds read them from.
use crate::{board_manifest::BoardManifest, profile_source::ProfileSource};
use fstart_image_build::amd::blobs::{self, BoardBlobs, SocBlobs};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// The name of the receipt `extract` writes into the blob directory.
pub const RECEIPT: &str = "fstart-blobs.toml";

/// Extract the board's firmware files from `rom` into `out`. Files the
/// image doesn't have are an error unless `allow_missing`.
pub fn extract(
    root: &Path,
    board: &BoardManifest,
    rom: &Path,
    out: &Path,
    allow_missing: bool,
) -> Result<(), String> {
    let source = crate::profile_source::load_profile(root, board)?;
    extract_from(&source, rom, out, allow_missing)?;
    eprintln!("[fstart] build with --blobs {}", out.display());
    Ok(())
}

/// The blob directory of a build from the donor image `rom` (`--donor`):
/// its files extracted once per board and image content, under
/// `target/fstart-blobs/<board>/<image SHA-256>/`.
pub(crate) fn from_donor(
    root: &Path,
    source: &ProfileSource,
    rom: &Path,
) -> Result<PathBuf, String> {
    let image = fs::read(rom).map_err(|e| format!("{}: {e}", rom.display()))?;
    let board = source.package["metadata"]["fstart"]["board"]
        .as_str()
        .ok_or("the board has no name")?;
    let digest = format!("{:x}", Sha256::digest(&image));
    let dir = root
        .join("target/fstart-blobs")
        .join(board)
        .join(&digest[..16]);
    if dir.join(RECEIPT).is_file() {
        eprintln!("[fstart] blobs of {} in {}", rom.display(), dir.display());
        return Ok(dir);
    }
    // Extract next to it, then publish: a failed extraction leaves no
    // directory a later build would take as complete.
    let partial = dir.with_extension("partial");
    let _ = fs::remove_dir_all(&partial);
    extract_from(source, rom, &partial, false)?;
    let _ = fs::remove_dir_all(&dir);
    fs::rename(&partial, &dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(dir)
}

fn extract_from(
    source: &ProfileSource,
    rom: &Path,
    out: &Path,
    allow_missing: bool,
) -> Result<(), String> {
    let table = &source.package["metadata"]["fstart"];
    let board_blobs: BoardBlobs = serde_json::from_value(table["amd"].clone())
        .map_err(|e| format!("board [package.metadata.fstart.amd]: {e}"))?;
    let soc: SocBlobs = serde_json::from_value(
        source.platform["metadata"]["fstart"]["amd"][&board_blobs.soc].clone(),
    )
    .map_err(|e| {
        format!(
            "platform [package.metadata.fstart.amd.{}]: {e}",
            board_blobs.soc
        )
    })?;
    let name = table["board"].as_str().ok_or("the board has no name")?;
    let image = fs::read(rom).map_err(|e| format!("{}: {e}", rom.display()))?;
    let (found, missing) = blobs::extract(&soc, &board_blobs, name, &image)?;
    if !missing.is_empty() && !allow_missing {
        return Err(format!(
            "{} does not have: {}",
            rom.display(),
            missing.join(", ")
        ));
    }
    let sha256 = |bytes: &[u8]| format!("{:x}", Sha256::digest(bytes));
    let quote = |s: &str| serde_json::to_string(s).unwrap();
    let mut receipt = format!(
        "# Written by `cargo fbuild blobs`.\nsource = {}\nsource-sha256 = {}\nboard = {}\nmissing = [{}]\n",
        quote(&rom.display().to_string()),
        quote(&sha256(&image)),
        quote(name),
        missing
            .iter()
            .map(|m| quote(m))
            .collect::<Vec<_>>()
            .join(", ")
    );
    for file in &found {
        let path = out.join(&file.file);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        fs::write(&path, &file.bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        eprintln!(
            "[fstart] {} ({} bytes) from {}",
            file.file,
            file.bytes.len(),
            file.origin
        );
        receipt += &format!(
            "\n[[file]]\npath = {}\nsha256 = {}\norigin = {}\n",
            quote(&file.file),
            quote(&sha256(&file.bytes)),
            quote(&file.origin)
        );
    }
    for file in &missing {
        eprintln!("[fstart] warning: {file} not in {}", rom.display());
    }
    let receipt_path = out.join(RECEIPT);
    fs::write(&receipt_path, receipt).map_err(|e| format!("{}: {e}", receipt_path.display()))?;
    eprintln!("[fstart] {} files in {}", found.len(), out.display());
    Ok(())
}
