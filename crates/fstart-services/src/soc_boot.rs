//! SoC boot header abstraction.
//!
//! Different SoCs use different boot header formats to communicate
//! boot-source selection and next-stage metadata from the BROM to the
//! firmware.  This trait provides a uniform interface so the executor
//! and board adapter can work with any SoC without hardcoding format
//! details.
//!
//! Current implementations:
//! - `fstart-soc-sunxi` (Allwinner eGON) — reads the eGON header's
//!   `boot_media`, `next_stage_offset`, and `next_stage_size` fields
//!   from the SRAM base address.
//!
//! To add a new SoC: implement this trait in a new `fstart-soc-*`
//! crate, add a `SocImageFormat` variant in `fstart-types`, and
//! teach `board_gen` to emit the qualified calls.

/// Boot-source detection and next-stage metadata from the SoC BROM.
///
/// Implemented by SoC crates (e.g., `fstart-soc-sunxi`).  The board
/// adapter calls these methods from its `boot_media_select` and
/// `load_next_stage` trampolines.
pub trait SocBootHeader {
    /// Read the hardware boot-source register value.
    ///
    /// The returned byte is matched against `BootMediaCandidate::media_ids`
    /// to select the active boot device.
    ///
    /// # Arguments
    /// * `header_base` — base address of the SoC boot header in memory
    ///   (e.g., the eGON header's SRAM address on sunxi).
    fn boot_media_at(header_base: usize) -> u8;

    /// Read the next-stage FFS offset from the boot header.
    ///
    /// Returns 0 if the header doesn't carry next-stage metadata.
    fn next_stage_offset_at(header_base: usize) -> u32;

    /// Read the next-stage binary size from the boot header.
    ///
    /// Returns 0 if the header doesn't carry next-stage metadata.
    fn next_stage_size_at(header_base: usize) -> u32;
}
