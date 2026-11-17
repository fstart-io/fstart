//! Firmware files extracted from a donor (vendor) flash image for
//! `fbuild blobs` and `--donor`: the one dispatch on the firmware-table
//! namespace a board declares in `[package.metadata.fstart]`. Each image
//! format that packs vendor firmware owns its namespace and extraction;
//! callers never name a format.
pub use crate::amd::blobs::{Donated as Extraction, Extracted};

/// Extract every file the board's and its platform's `fstart` manifest
/// tables (`board_table`, `platform_table`) name from the donor image
/// `rom`. A SoC the donor has no firmware of is listed as absent; missing
/// files of a SoC it has are an error unless `allow_missing`.
pub fn extract(
    board_table: &serde_json::Value,
    platform_table: &serde_json::Value,
    rom: &[u8],
    allow_missing: bool,
) -> Result<Extraction, String> {
    if board_table["amd"].is_null() {
        return Err("the board declares no donor-extractable firmware tables".into());
    }
    crate::amd::blobs::extract_tables(board_table, platform_table, rom, allow_missing)
}
