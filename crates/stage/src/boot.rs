//! Loading for stages: the chain lives in `fstart-load` (read → verify →
//! decompress → place); this module re-exports it and adds the running
//! stage's own reservations.

use fstart_core::services::BootMedia;
pub use fstart_load::{
    Codec, LoadError, Lz4, MemoryPolicy, MemoryWindow, Stored, VerifiedExecutable,
    bootstrap_footprint, load_bootstrap, load_bootstrap_into, place, stored_input,
};

/// Linker-owned running image and writable state reservations. Includes XIP
/// text/rodata/data source separately from live data/BSS/heap/stack/page tables.
/// Boards must still reserve handoff, temporary arenas, tables and DMA buffers
/// located outside these linker ranges.
pub fn running_stage_windows() -> Result<[MemoryWindow; 2], LoadError> {
    #[cfg(target_os = "none")]
    {
        unsafe extern "C" {
            static _text_start: u8;
            static _data_load: u8;
            static _data_start: u8;
            static _data_end: u8;
            static _writable_end: u8;
        }
        let text = core::ptr::addr_of!(_text_start) as u64;
        let data_load = core::ptr::addr_of!(_data_load) as u64;
        let data_start = core::ptr::addr_of!(_data_start) as u64;
        let data_end = core::ptr::addr_of!(_data_end) as u64;
        let writable_end = core::ptr::addr_of!(_writable_end) as u64;
        let initialized = data_end.checked_sub(data_start).ok_or(LoadError::Bounds)?;
        let image_end = data_load
            .checked_add(initialized)
            .ok_or(LoadError::Bounds)?;
        Ok([
            MemoryWindow {
                start: text,
                size: image_end.checked_sub(text).ok_or(LoadError::Bounds)?,
            },
            MemoryWindow {
                start: data_start,
                size: writable_end
                    .checked_sub(data_start)
                    .ok_or(LoadError::Bounds)?,
            },
        ])
    }
    #[cfg(not(target_os = "none"))]
    {
        Err(LoadError::MemoryPolicy)
    }
}

/// Read an exact range of not yet verified bytes.
pub(crate) fn read_exact(
    media: &(impl BootMedia + ?Sized),
    offset: u64,
    destination: &mut [u8],
) -> Result<(), LoadError> {
    fstart_load::read(media, offset, destination).map(drop)
}
