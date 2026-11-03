//! A bounded, writable region of the firmware flash.
//!
//! Data that firmware keeps across boots (memory-training caches, variable
//! stores) lives in a reserved region of the boot flash. Its driver binds
//! the region at construction and enforces the bounds, the part's geometry
//! and its protection; users see offsets relative to the region only.

use super::ServiceError;

/// Read, program and erase within one reserved flash region. Every offset
/// is relative to the region; the implementation rejects anything outside
/// it and never unlocks unrelated parts of the flash.
pub trait FlashRegion {
    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), ServiceError>;
    /// Program erased bytes (NOR semantics: bits only go from 1 to 0).
    fn program(&mut self, offset: u32, bytes: &[u8]) -> Result<(), ServiceError>;
    /// Erase whole erase blocks of the part to `0xff`.
    fn erase(&mut self, offset: u32, length: u32) -> Result<(), ServiceError>;
}
