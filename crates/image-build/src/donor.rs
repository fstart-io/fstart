//! Firmware files extracted from a donor (vendor) flash image for
//! `fbuild blobs`, `--donor` and a bundle's assembly (natively and in the
//! browser): the one dispatch on the firmware-table namespace a board
//! declares in `[package.metadata.fstart]`. Each image format that packs
//! vendor firmware owns its namespace and extraction; callers never name a
//! format.
//!
//! A donor is a flash image, or a vendor update ZIP holding one ([`image`]).
pub use crate::amd::blobs::{Donated as Extraction, Extracted};
use std::{borrow::Cow, io::Cursor, path::Path};

pub const MAX_DONOR: u64 = 128 << 20;
/// Copies of the donor image a vendor ZIP may hold (one per flash tool).
pub const MAX_DONOR_COPIES: usize = 4;

/// The flash image of a donor file.
#[derive(Debug)]
pub struct Image<'a> {
    pub bytes: Cow<'a, [u8]>,
    /// The ZIP member it is, if the donor is a ZIP.
    pub member: Option<String>,
}

/// The flash image in `donor`: the file itself, or the flash image of a
/// vendor update ZIP. Of a ZIP's ROM, bin and fd members (1 MiB or more),
/// those with firmware the board takes (`usable`) count, so its capsules,
/// tools and images of other boards do not; copies of one image (one per
/// flash tool) are one. A ZIP of several such images must be unpacked by
/// the user.
pub fn image<'a>(donor: &'a [u8], usable: &dyn Fn(&[u8]) -> bool) -> Result<Image<'a>, String> {
    if donor.len() as u64 > MAX_DONOR {
        return Err("donor too large".into());
    }
    // A ZIP starts with a local file header, or is empty (end of directory).
    if !donor.starts_with(b"PK\x03\x04") && !donor.starts_with(b"PK\x05\x06") {
        return Ok(Image {
            bytes: donor.into(),
            member: None,
        });
    }
    let mut zip = zip::ZipArchive::new(Cursor::new(donor)).map_err(|e| e.to_string())?;
    let candidates: Vec<(String, u64)> = (0..zip.len())
        .filter_map(|index| {
            let file = zip.by_index(index).ok()?;
            let name = file.name().to_owned();
            let ext = Path::new(&name).extension()?.to_str()?.to_ascii_lowercase();
            ((1 << 20..=MAX_DONOR).contains(&file.size())
                && matches!(ext.as_str(), "bin" | "rom" | "fd"))
            .then_some((name, file.size()))
        })
        .collect();
    // A ZIP of many large members must not make us expand them all.
    if candidates.len() > MAX_DONOR_COPIES {
        return Err("donor ZIP holds too many ROM/bin/fd images".into());
    }
    if candidates.is_empty() {
        return Err("donor ZIP holds no ROM/bin/fd image".into());
    }
    let mut images: Vec<(String, Vec<u8>)> = Vec::new();
    for (name, size) in candidates {
        let bytes = crate::bundle::read_member(&mut zip, &name, size)?;
        if usable(&bytes) && images.iter().all(|(_, image)| *image != bytes) {
            images.push((name, bytes));
        }
    }
    match images.len() {
        0 => Err("donor ZIP holds no ROM/bin/fd image with firmware for this board".into()),
        1 => {
            let (name, bytes) = images.remove(0);
            Ok(Image {
                bytes: bytes.into(),
                member: Some(name),
            })
        }
        _ => Err(format!(
            "donor ZIP holds different ROM/bin/fd images ({}); unpack and select the ROM",
            images
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Extract every file the board's and its platform's `fstart` manifest
/// tables (`board_table`, `platform_table`) name from the donor `donor`
/// (a flash image or a vendor ZIP, [`image`]). A SoC the donor has no
/// firmware of is listed as absent; missing files of a SoC it has are an
/// error unless `allow_missing`.
pub fn extract(
    board_table: &serde_json::Value,
    platform_table: &serde_json::Value,
    donor: &[u8],
    allow_missing: bool,
) -> Result<Extraction, String> {
    if board_table["amd"].is_null() {
        return Err("the board declares no donor-extractable firmware tables".into());
    }
    let name = board_table["board"]
        .as_str()
        .ok_or("the board has no name")?;
    let tables = crate::amd::blobs::tables(board_table, platform_table)?;
    let image = image(donor, &|image| {
        crate::amd::blobs::has_firmware(&tables, image)
    })?;
    let mut extraction =
        crate::amd::blobs::extract_donor(&tables, name, &image.bytes, allow_missing)?;
    extraction.member = image.member;
    Ok(extraction)
}

#[cfg(test)]
#[path = "donor_tests.rs"]
pub(crate) mod tests;
