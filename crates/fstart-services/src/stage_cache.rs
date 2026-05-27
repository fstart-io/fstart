//! Firmware stage-cache provider trait.

use crate::ServiceError;

/// Provides a firmware-owned region for compressed stage cache storage.
pub trait StageCacheProvider: Send + Sync {
    /// Return `(base, size)` for a cache region with at least `requested_size` bytes.
    fn stage_cache_region(&self, requested_size: u64) -> Option<(u64, u64)>;

    /// Make the cache region writable/readable by normal firmware.
    fn stage_cache_open(&self) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Close the cache region after access.
    fn stage_cache_close(&self) -> Result<(), ServiceError> {
        Ok(())
    }
}
