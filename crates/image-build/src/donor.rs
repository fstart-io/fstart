//! Firmware files extracted from a donor (vendor) flash image for
//! `fbuild blobs` and `--donor`: the one dispatch on the firmware-table
//! namespace a board declares in `[package.metadata.fstart]`. Each image
//! format that packs vendor firmware owns its namespace and extraction;
//! callers never name a format.
pub use crate::amd::blobs::Extracted;

/// What a donor image yields for a board.
#[derive(Debug)]
pub struct Extraction {
    /// The board's name, as its table states it.
    pub board: String,
    pub found: Vec<Extracted>,
    /// The files the image does not have.
    pub missing: Vec<String>,
}

/// Extract every file the board's and its platform's `fstart` manifest
/// tables (`board_table`, `platform_table`) name from the donor image
/// `rom`. Missing files are an error unless `allow_missing`.
pub fn extract(
    board_table: &serde_json::Value,
    platform_table: &serde_json::Value,
    rom: &[u8],
    allow_missing: bool,
) -> Result<Extraction, String> {
    let (board, found, missing) = if !board_table["amd"].is_null() {
        crate::amd::blobs::extract_tables(board_table, platform_table, rom, allow_missing)?
    } else {
        return Err("the board declares no donor-extractable firmware tables".into());
    };
    Ok(Extraction {
        board,
        found,
        missing,
    })
}
