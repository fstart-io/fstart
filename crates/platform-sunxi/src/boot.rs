//! Sunxi's ROM loading path has no established protected trust anchor.
//! The initial-image pin detects nextstage corruption, not whole-image replacement.

use fstart_core::services::boot_media::BlockDeviceMedia;
use fstart_core::services::{BlockDevice, ServiceError};
use fstart_stage::boot::{MemoryPolicy, MemoryWindow};

/// Load only the descriptor embedded in this SPL, never eGON's unsigned offsets.
/// The MMC drivers use synchronous programmed I/O, not DMA.
pub(crate) fn load_mainstage(
    block: &impl BlockDevice,
    media_base: u64,
    image_size: usize,
    dram_base: u64,
    dram_size: u64,
    load_addr: u64,
    handoff_addr: u64,
) -> Result<u64, ServiceError> {
    fstart_log::info!(
        "sunxi: development integrity; no hardware secure boot or rollback enforcement"
    );
    validate_dram(dram_base, dram_size, handoff_addr)?;
    let descriptor = fstart_stage::next_stage::pinned_bootstrap_descriptor()
        .map_err(|_| ServiceError::InvalidParam)?;
    if descriptor.load_addr != load_addr || dram_size == 0 {
        return Err(ServiceError::InvalidParam);
    }
    let media = BlockDeviceMedia::new(block, media_base, image_size);
    let writable = [MemoryWindow {
        start: dram_base,
        size: dram_size,
    }];
    let reserved = [MemoryWindow {
        start: handoff_addr,
        size: fstart_core::handoff::HANDOFF_MAX_SIZE as u64,
    }];
    let policy = MemoryPolicy {
        writable: &writable,
        reserved: &reserved,
        entry_alignment: 4,
    };
    // SAFETY: SPL code, stack and descriptor live in SRAM, disjoint from detected
    // DRAM. Reserve the surviving handoff; the synchronous MMC driver has no DMA.
    let executable = unsafe { fstart_stage::boot::load_bootstrap(&media, &descriptor, &policy) }
        .map_err(|_| ServiceError::InvalidParam)?;
    Ok(executable.entry())
}

fn validate_dram(base: u64, size: u64, handoff: u64) -> Result<(), ServiceError> {
    let end = base.checked_add(size).ok_or(ServiceError::InvalidParam)?;
    let handoff_end = handoff
        .checked_add(fstart_core::handoff::HANDOFF_MAX_SIZE as u64)
        .ok_or(ServiceError::InvalidParam)?;
    if base != 0x4000_0000 || size == 0 || end > 0x8000_0000 || handoff < base || handoff_end > end
    {
        return Err(ServiceError::InvalidParam);
    }
    Ok(())
}

/// Retain a DRAM policy before any block-backed FFS consumer can write memory.
#[cfg(feature = "linux")]
pub(crate) fn install_mainstage_policy(
    dram_base: u64,
    dram_size: u64,
    handoff_addr: u64,
    fdt_destination: u64,
) -> Result<(), ServiceError> {
    validate_dram(dram_base, dram_size, handoff_addr)?;
    let writable = [MemoryWindow {
        start: dram_base,
        size: dram_size,
    }];
    let reserved =
        fstart_stage::boot::running_stage_windows().map_err(|_| ServiceError::InvalidParam)?;
    let handoff = MemoryWindow {
        start: handoff_addr,
        size: fstart_core::handoff::HANDOFF_MAX_SIZE as u64,
    };
    let reservations = [reserved[0], reserved[1], handoff];
    let policy = MemoryPolicy {
        writable: &writable,
        reserved: &reservations,
        entry_alignment: 4,
    };
    // SAFETY: running image/heap/stack and handoff are excluded. MMC is PIO.
    unsafe { fstart_stage::directory::set_load_policy(&policy) }
        .map_err(|_| ServiceError::InvalidParam)?;
    let workspace = MemoryWindow {
        start: fdt_destination,
        size: 64 * 1024,
    };
    // SAFETY: registration checks this dedicated buffer against detected RAM
    // and the live reservations. FFS authenticates/relocates the DTB here before
    // it is parsed; no external source or unbounded whole-RAM view is granted.
    unsafe { fstart_stage::configure_fdt_workspace(workspace, workspace) }
}
