//! LoadNextStage runtime helpers.
//!
//! Extracts the handoff serialization, block device read, and jump
//! logic from codegen into testable library functions. Codegen still
//! handles SoC-specific header parsing (eGON offsets) and multi-device
//! match dispatch, but calls these functions for the actual work.

use fstart_services::{BlockDevice, ServiceError};

/// Read a firmware stage from a block device directly to its load address.
///
/// Performs a single block device read of `size` bytes from `offset`
/// into `load_addr`. Returns the number of bytes read on success.
///
/// # Safety
///
/// Caller must ensure `load_addr` points to writable RAM with at least
/// `size` bytes available. This is guaranteed by the board config and
/// linker script.
pub fn read_stage_to_addr(
    dev: &impl BlockDevice,
    dev_name: &str,
    next_stage: &str,
    offset: u64,
    load_addr: u64,
    size: usize,
) -> Result<usize, ServiceError> {
    fstart_log::info!(
        "loading stage '{}' from {}: offset={:#x}, size={:#x}, dest={:#x}",
        next_stage,
        dev_name,
        offset,
        size as u64,
        load_addr
    );
    // SAFETY: load_addr points to writable RAM per board config.
    let dest_buf = unsafe { core::slice::from_raw_parts_mut(load_addr as *mut u8, size) };
    dev.read(offset, dest_buf)
}

/// Serialize handoff data to a DRAM buffer for the next stage.
///
/// Writes a [`StageHandoff`](fstart_types::handoff::StageHandoff) to
/// `handoff_addr` and returns the number of bytes written.
///
/// # Safety
///
/// Caller must ensure `handoff_addr` points to writable RAM with at
/// least [`HANDOFF_MAX_SIZE`](fstart_types::handoff::HANDOFF_MAX_SIZE)
/// bytes. This is guaranteed by placing the handoff buffer at a known
/// offset below the next stage's load address.
///
/// # Errors
///
/// Returns `Err` if postcard serialization fails (buffer too small or
/// encoding error).
#[cfg(feature = "handoff")]
pub fn serialize_handoff(dram_size: u64, handoff_addr: u64) -> Result<usize, &'static str> {
    let handoff_data = fstart_types::handoff::StageHandoff::new(dram_size);
    // SAFETY: handoff_addr points to writable RAM, 4K below next stage load_addr.
    let handoff_buf = unsafe {
        core::slice::from_raw_parts_mut(
            handoff_addr as *mut u8,
            fstart_types::handoff::HANDOFF_MAX_SIZE,
        )
    };
    let handoff_len = crate::handoff::serialize(&handoff_data, handoff_buf)?;
    fstart_log::info!("handoff: {} bytes at {:#x}", handoff_len, handoff_addr);
    Ok(handoff_len)
}
