//! Block device service — storage abstraction.

use crate::ServiceError;

/// A block device (flash, disk, etc.).
pub trait BlockDevice: Send + Sync {
    /// Read bytes starting at `offset` into `buf`. Returns bytes read.
    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, ServiceError>;

    /// Write `buf` starting at `offset`. Returns bytes written.
    fn write(&self, offset: u64, buf: &[u8]) -> Result<usize, ServiceError>;

    /// Erase a region (for flash devices). Default: not supported.
    fn erase(&self, _offset: u64, _size: u64) -> Result<(), ServiceError> {
        Err(ServiceError::NotSupported)
    }

    /// Total device size in bytes.
    fn size(&self) -> u64;

    /// Smallest writable unit in bytes.
    fn block_size(&self) -> u32 {
        1
    }
}
