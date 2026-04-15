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

// ---------------------------------------------------------------------------
// Global e820 state — populated by MemoryDetect, read by PCI host bridges
// ---------------------------------------------------------------------------

/// Maximum number of e820 entries stored in the global state.
pub const MAX_E820_ENTRIES: usize = 128;

/// Shared e820 memory map state.
///
/// Populated by the `MemoryDetect` capability after calling
/// `MemoryDetector::detect_memory()`.  Read by PCI host bridge drivers
/// (e.g., Q35) to compute MMIO windows without requiring the codegen to
/// pass e820 data explicitly.
///
/// This is firmware-level global state: single-threaded, set once during
/// the capability pipeline, then read-only.
pub struct E820State {
    entries: [E820Entry; MAX_E820_ENTRIES],
    count: usize,
    total_ram: u64,
}

impl E820State {
    const fn new() -> Self {
        Self {
            entries: [E820Entry::zeroed(); MAX_E820_ENTRIES],
            count: 0,
            total_ram: 0,
        }
    }

    /// Store e820 entries and total RAM. Called once by MemoryDetect.
    ///
    /// # Safety
    ///
    /// Must be called from single-threaded firmware init context.
    pub unsafe fn store(&mut self, entries: &[E820Entry], count: usize, total_ram: u64) {
        let n = count.min(MAX_E820_ENTRIES);
        self.entries[..n].copy_from_slice(&entries[..n]);
        self.count = n;
        self.total_ram = total_ram;
    }

    /// Get the stored e820 entries.
    pub fn entries(&self) -> &[E820Entry] {
        &self.entries[..self.count]
    }

    /// Get the stored entry count.
    pub fn count(&self) -> usize {
        self.count
    }

    /// Get the total detected RAM in bytes.
    pub fn total_ram(&self) -> u64 {
        self.total_ram
    }
}

/// Global e820 state instance.
///
/// # Safety
///
/// Access is safe in single-threaded firmware init. The `store()` method
/// is called once during MemoryDetect; subsequent reads via `e820_state()`
/// are safe because no concurrent mutation occurs.
static mut E820_GLOBAL: E820State = E820State::new();

/// Get a shared reference to the global e820 state.
///
/// # Safety
///
/// Safe to call after `MemoryDetect` has completed (which populates the
/// state). Must not be called concurrently with `store()`.
pub unsafe fn e820_state() -> &'static E820State {
    unsafe { &*core::ptr::addr_of!(E820_GLOBAL) }
}

/// Get a mutable reference to the global e820 state for initial population.
///
/// # Safety
///
/// Must only be called once, from the MemoryDetect capability, in
/// single-threaded firmware init context.
pub unsafe fn e820_state_mut() -> &'static mut E820State {
    unsafe { &mut *core::ptr::addr_of_mut!(E820_GLOBAL) }
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
