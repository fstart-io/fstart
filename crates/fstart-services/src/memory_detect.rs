//! Memory detection service trait.
//!
//! Implemented by devices that can discover the system memory layout
//! at runtime, such as QEMU's fw_cfg device (which provides an e820 map)
//! or future SPD/memory-training drivers.

use crate::ServiceError;

/// e820 memory region types.
///
/// These values match the x86 e820 / ACPI AddressRangeDescriptor types
/// and are used regardless of architecture (the same enum can feed
/// FDT `/memory` node updates on ARM/RISC-V).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum E820Kind {
    /// Usable RAM.
    Ram = 1,
    /// Reserved by firmware / hardware.
    Reserved = 2,
    /// ACPI reclaimable memory (usable after ACPI tables are read).
    Acpi = 3,
    /// ACPI Non-Volatile Storage.
    Nvs = 4,
    /// Unusable / defective memory.
    Unusable = 5,
}

/// A single memory region entry (matches the x86 e820 layout).
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct E820Entry {
    /// Physical start address of the region.
    pub addr: u64,
    /// Size of the region in bytes.
    pub size: u64,
    /// Region type.
    pub kind: u32,
}

impl E820Entry {
    /// Create a zeroed (invalid) entry.
    pub const fn zeroed() -> Self {
        Self {
            addr: 0,
            size: 0,
            kind: 0,
        }
    }

    /// Create a new entry.
    pub const fn new(addr: u64, size: u64, kind: E820Kind) -> Self {
        Self {
            addr,
            size,
            kind: kind as u32,
        }
    }
}

/// A device that can detect the system memory layout at runtime.
pub trait MemoryDetector {
    /// Discover memory regions and write them to `entries`.
    ///
    /// Returns the number of entries written. The caller provides a
    /// buffer of at least 128 entries (the x86 e820 protocol maximum).
    fn detect_memory(&self, entries: &mut [E820Entry]) -> Result<usize, ServiceError>;

    /// Return the total usable RAM in bytes.
    ///
    /// This is the sum of all `E820Kind::Ram` regions. Implementations
    /// may compute this from `detect_memory()` results or from a
    /// separate query (e.g., fw_cfg `FW_CFG_RAM_SIZE`).
    fn total_ram_bytes(&self) -> Result<u64, ServiceError>;
}
