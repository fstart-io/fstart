//! Raminit's DRAM inventory, handed from the CAR stage to the SMBIOS writer.

use fstart_core::layout::Region;
use fstart_core::memory_info::MemoryInfo;
#[cfg(fstart_stage_env = "car")]
use fstart_core::services::ServiceError;

/// Publish the inventory, or clear a record left in DRAM by a previous boot.
///
/// # Safety
/// `region` must be the linked, firmware-owned `MemoryInfo` reservation and
/// lie in trained DRAM.
#[cfg(fstart_stage_env = "car")]
pub(crate) unsafe fn publish(
    region: Region,
    info: Option<&MemoryInfo>,
) -> Result<(), ServiceError> {
    use zerocopy::IntoBytes;
    let bytes = info.map_or(&[0u8; size_of::<MemoryInfo>()][..], |info| info.as_bytes());
    if region.size < bytes.len() as u64 {
        return Err(ServiceError::InvalidParam);
    }
    let base = region.base as *mut u8;
    // SAFETY: the caller guarantees the reservation is writable DRAM.
    unsafe {
        for (offset, byte) in bytes.iter().enumerate() {
            core::ptr::write_volatile(base.add(offset), *byte);
        }
        // A chipset memory test may already have enabled WB caching of DRAM;
        // CAR teardown's INVD must not discard the record.
        #[cfg(target_arch = "x86_64")]
        {
            core::arch::asm!("mfence", options(nostack, preserves_flags));
            for offset in (0..bytes.len()).step_by(64) {
                core::arch::x86_64::_mm_clflush(base.add(offset));
            }
            core::arch::asm!("mfence", options(nostack, preserves_flags));
        }
    }
    Ok(())
}

/// The inventory raminit published on this boot, if any.
#[cfg(fstart_stage_env = "ram")]
pub(crate) fn read(region: Region) -> Option<MemoryInfo> {
    if region.size < size_of::<MemoryInfo>() as u64 {
        return None;
    }
    // SAFETY: the linked reservation is firmware-owned DRAM that the CAR
    // stage wrote (or cleared) before this stage was loaded.
    let bytes =
        unsafe { core::slice::from_raw_parts(region.base as *const u8, size_of::<MemoryInfo>()) };
    MemoryInfo::from_bytes(bytes).copied()
}
