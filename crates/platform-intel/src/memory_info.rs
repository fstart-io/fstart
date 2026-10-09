//! Raminit's DRAM inventory, handed from the CAR stage to the SMBIOS writer
//! through the firmware store.

use fstart_core::memory_info::MemoryInfo;
use fstart_store::{Store, tag};

/// Publish the inventory raminit reports, if any. A resume reuses the entry
/// of the cold boot.
#[cfg(fstart_stage_env = "car")]
pub(crate) fn publish(
    store: &mut Store,
    info: Option<&MemoryInfo>,
) -> Result<(), fstart_core::services::ServiceError> {
    use zerocopy::IntoBytes;
    let Some(info) = info else {
        return Ok(());
    };
    let bytes = info.as_bytes();
    let entry = store
        .find(tag::MEMORY_INFO)
        .filter(|entry| entry.len() == bytes.len())
        .map_or_else(|| store.add(tag::MEMORY_INFO, bytes.len(), 3), Ok)
        .map_err(|_| fstart_core::services::ServiceError::InvalidParam)?;
    // SAFETY: a store entry owned by the inventory.
    unsafe { store.bytes_mut(&entry) }.copy_from_slice(bytes);
    Ok(())
}

/// The inventory raminit published, if any.
#[cfg(fstart_stage_env = "ram")]
pub(crate) fn read(store: &Store) -> Option<MemoryInfo> {
    let entry = store.find(tag::MEMORY_INFO)?;
    // SAFETY: the entry holds the record the CAR stage wrote.
    MemoryInfo::from_bytes(unsafe { store.bytes_mut(&entry) }).copied()
}
