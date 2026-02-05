//! Memory map types.

use heapless::String as HString;
use serde::{Deserialize, Serialize};

/// Complete memory map for a board.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryMap {
    /// Named memory regions
    pub regions: heapless::Vec<MemoryRegion, 16>,
    /// Stack size for firmware stages (bytes)
    pub stack_size: u32,
}

/// A single memory region.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRegion {
    /// Region name (e.g., "rom", "ram", "mmio")
    pub name: HString<32>,
    /// Base physical address
    pub base: u64,
    /// Size in bytes
    pub size: u64,
    /// What kind of memory this is
    pub kind: RegionKind,
}

/// Type of memory region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RegionKind {
    /// Read-only memory (flash, ROM)
    Rom,
    /// Read-write memory (DRAM, SRAM)
    Ram,
    /// Memory-mapped I/O
    Mmio,
    /// Reserved (firmware-owned, not passed to OS)
    Reserved,
}
