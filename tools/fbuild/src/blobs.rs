//! `fbuild blobs`: fill a blob directory from a vendor flash image with
//! the files the board's firmware tables name, at the paths `--blobs`
//! builds read them from. The extraction belongs to the image format
//! whose table namespace the board declares
//! ([`fstart_image_build::donor`]); this module writes the files and
//! their receipt.
use crate::{board_manifest::BoardManifest, profile_source::ProfileSource};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

/// The name of the receipt `extract` writes into the blob directory.
pub const RECEIPT: &str = "fstart-blobs.toml";

/// Extract the board's firmware files from `rom` (a flash image, or a
/// vendor update ZIP holding one) into `out`. Files the image doesn't
/// have are an error unless `allow_missing`.
pub fn extract(
    root: &Path,
    board: &BoardManifest,
    rom: &Path,
    out: &Path,
    allow_missing: bool,
) -> Result<(), String> {
    let source = crate::profile_source::load_profile(root, board)?;
    extract_from(&source, rom, &read_donor(rom)?, out, allow_missing)?;
    eprintln!("[fstart] build with --blobs {}", out.display());
    Ok(())
}

/// The blob directory of a build from the donor `rom` (`--donor`, a flash
/// image or a vendor update ZIP): its files extracted once per board,
/// donor content and blob tables (the board's and its platform's), under
/// `target/fstart-blobs/<board>/<donor SHA-256>-<tables SHA-256>/`. The
/// plan names it by none of that ([`fstart_image_build::files::BLOBS_DIR`]).
pub(crate) fn from_donor(
    root: &Path,
    source: &ProfileSource,
    rom: &Path,
) -> Result<PathBuf, String> {
    let image = read_donor(rom)?;
    let board = source.package["metadata"]["fstart"]["board"]
        .as_str()
        .ok_or("the board has no name")?;
    let digest = format!("{:x}", Sha256::digest(&image));
    // A changed table names other files: extract again.
    let tables = serde_json::to_vec(&[
        &source.package["metadata"]["fstart"],
        &source.platform["metadata"]["fstart"],
    ])
    .map_err(|e| e.to_string())?;
    let tables = format!("{:x}", Sha256::digest(&tables));
    let dir = root.join("target/fstart-blobs").join(board).join(format!(
        "{}-{}",
        &digest[..16],
        &tables[..8]
    ));
    if dir.join(RECEIPT).is_file() {
        eprintln!("[fstart] blobs of {} in {}", rom.display(), dir.display());
        return Ok(dir);
    }
    // Extract next to it, then publish: a failed extraction leaves no
    // directory a later build would take as complete.
    let partial = dir.with_extension("partial");
    let _ = fs::remove_dir_all(&partial);
    extract_from(source, rom, &image, &partial, false)?;
    let _ = fs::remove_dir_all(&dir);
    fs::rename(&partial, &dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(dir)
}

/// The donor file `rom`, read once and no further than a donor may be
/// ([`fstart_image_build::donor::MAX_DONOR`]).
pub(crate) fn read_donor(rom: &Path) -> Result<Vec<u8>, String> {
    use fstart_image_build::donor::MAX_DONOR;
    let error = |e: std::io::Error| format!("{}: {e}", rom.display());
    let file = fs::File::open(rom).map_err(error)?;
    let too_large = || format!("{}: donor larger than {MAX_DONOR} bytes", rom.display());
    if file.metadata().map_err(error)?.len() > MAX_DONOR {
        return Err(too_large());
    }
    let mut image = Vec::new();
    file.take(MAX_DONOR + 1)
        .read_to_end(&mut image)
        .map_err(error)?;
    if image.len() as u64 > MAX_DONOR {
        return Err(too_large());
    }
    Ok(image)
}

/// Extract the board's files from `image`, the donor `rom`, into `out`.
fn extract_from(
    source: &ProfileSource,
    rom: &Path,
    image: &[u8],
    out: &Path,
    allow_missing: bool,
) -> Result<(), String> {
    let table = &source.package["metadata"]["fstart"];
    let fstart_image_build::donor::Extraction {
        board: name,
        member,
        found,
        missing,
        absent,
    } = fstart_image_build::donor::extract(
        table,
        &source.platform["metadata"]["fstart"],
        image,
        allow_missing,
    )
    .map_err(|e| format!("{}: {e}", rom.display()))?;
    let name = name.as_str();
    if !missing.is_empty() && !allow_missing {
        return Err(format!(
            "{} does not have: {}",
            rom.display(),
            missing.join(", ")
        ));
    }
    let sha256 = |bytes: &[u8]| format!("{:x}", Sha256::digest(bytes));
    let quote = |s: &str| serde_json::to_string(s).unwrap();
    if let Some(member) = &member {
        eprintln!("[fstart] {member} of {}", rom.display());
    }
    let mut receipt = format!(
        "# Written by `cargo fbuild blobs`.\nsource = {}\nsource-sha256 = {}\n{}board = {}\nmissing = [{}]\n",
        quote(&rom.display().to_string()),
        quote(&sha256(image)),
        member.as_deref().map_or_else(String::new, |member| format!(
            "source-member = {}\n",
            quote(member)
        )),
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
    for soc in &absent {
        eprintln!(
            "[fstart] {} has no {soc} firmware: the image packs the others",
            rom.display()
        );
    }
    let receipt_path = out.join(RECEIPT);
    fs::write(&receipt_path, receipt).map_err(|e| format!("{}: {e}", receipt_path.display()))?;
    eprintln!("[fstart] {} files in {}", found.len(), out.display());
    Ok(())
}
