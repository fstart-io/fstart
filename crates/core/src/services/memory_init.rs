//! Memory-init block.

use super::ServiceError;
use crate::description::Range;

/// Bring DRAM up and report where it is.
///
/// Implemented by DRAM controllers whose configuration comes from the board
/// (timings, ODT, bus width): they program the controller, train it and size
/// the memory. Memory that came up before fstart ran (PSP, a vendor ROM,
/// QEMU) needs no training; its block only reports the range.
///
/// DIMM-based chipsets that read SPDs and replay cached training take more
/// than this (an SMBus, the cache) and keep a richer contract of their own.
pub trait MemoryInit {
    /// Train or find the memory and return the DRAM range it occupies.
    fn bring_up(&mut self) -> Result<Range, ServiceError>;
}
