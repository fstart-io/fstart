//! Memory detection service trait.
//!
//! Implemented by devices that can discover the system memory layout
//! at runtime, such as QEMU's fw_cfg device (which provides an e820 map)
//! or future SPD/memory-training drivers.

use super::ServiceError;

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

/// Build a conventional PC-compatible e820 map from chipset-discovered RAM
/// limits.
///
/// The caller supplies the top of usable low memory, TOLUD/top of low DRAM
/// decode, and TOUUD/top of reclaim/high memory.  The helper emits standard
/// legacy holes plus RAM/reserved ranges suitable for ACPI/SMBIOS/payload
/// handoff.
pub fn build_pc_compatible_e820(
    entries: &mut [E820Entry],
    usable_low_top: u32,
    touud: u64,
    tolud: u32,
) -> Result<usize, ServiceError> {
    if entries.len() < 8 {
        return Err(ServiceError::HardwareError);
    }

    let mut count = 0usize;
    // Keep page zero and legacy PC holes out of the EFI/OS allocator. This
    // mirrors coreboot's handoff: page zero, EBDA, VGA/option-ROM space, and
    // BIOS ROM are all reserved.
    entries[count] = E820Entry::new(0x0000_0000, 0x0000_1000, E820Kind::Reserved);
    count += 1;
    entries[count] = E820Entry::new(0x0000_1000, 0x0009_e000, E820Kind::Ram);
    count += 1;
    entries[count] = E820Entry::new(0x0009_f000, 0x0000_1000, E820Kind::Reserved);
    count += 1;
    entries[count] = E820Entry::new(0x000a_0000, 0x0005_0000, E820Kind::Reserved);
    count += 1;
    entries[count] = E820Entry::new(0x000f_0000, 0x0001_0000, E820Kind::Reserved);
    count += 1;

    let usable_low_top = u64::from(usable_low_top).max(0x0010_0000);
    let low_ram_size = usable_low_top.saturating_sub(0x0010_0000);
    if low_ram_size != 0 {
        entries[count] = E820Entry::new(0x0010_0000, low_ram_size, E820Kind::Ram);
        count += 1;
    }

    let top_reserved_size = u64::from(tolud).saturating_sub(usable_low_top);
    if top_reserved_size != 0 {
        entries[count] = E820Entry::new(usable_low_top, top_reserved_size, E820Kind::Reserved);
        count += 1;
    }

    let upper_ram_size = touud.saturating_sub(0x1_0000_0000);
    if upper_ram_size != 0 {
        entries[count] = E820Entry::new(0x1_0000_0000, upper_ram_size, E820Kind::Ram);
        count += 1;
    }

    Ok(count)
}

// ---------------------------------------------------------------------------
// Global e820 state — populated by fixed memory init, read by PCI host bridges
// ---------------------------------------------------------------------------

/// Maximum number of e820 entries stored in the global state.
pub const MAX_E820_ENTRIES: usize = 128;

/// Shared e820 memory map state.
///
/// Populated by fixed memory init after calling
/// [`MemoryDetector::detect_memory`]. Read by PCI host bridge drivers (e.g.,
/// Q35) to compute MMIO windows without threading e820 data through every call.
///
/// This is firmware-level global state: single-threaded, set once during a
/// platform flow, then read-only.
pub struct E820State {
    entries: [E820Entry; MAX_E820_ENTRIES],
    count: usize,
    total_ram: u64,
}

impl Default for E820State {
    fn default() -> Self {
        Self::new()
    }
}

impl E820State {
    pub const fn new() -> Self {
        Self {
            entries: [E820Entry::zeroed(); MAX_E820_ENTRIES],
            count: 0,
            total_ram: 0,
        }
    }

    /// Raw entry storage for in-place population by a `MemoryDetector`.
    pub fn entries_mut(&mut self) -> &mut [E820Entry; MAX_E820_ENTRIES] {
        &mut self.entries
    }

    /// Record the detected entry count and total RAM after in-place
    /// population via [`entries_mut`](Self::entries_mut).
    pub fn set_detected(&mut self, count: usize, total_ram: u64) -> Result<(), ServiceError> {
        if count > MAX_E820_ENTRIES {
            return Err(ServiceError::InvalidParam);
        }
        self.count = count;
        self.total_ram = total_ram;
        Ok(())
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

    /// Carve a reserved range out of RAM entries in-place.
    ///
    /// RAM entries that overlap `[base, base + size)` are split into before /
    /// reserved / after pieces. Non-RAM entries are preserved. Zero-sized
    /// ranges are ignored. Intended for firmware-owned allocations (stage
    /// image, heap tables) before OS handoff.
    pub fn reserve_range(&mut self, base: u64, size: u64) -> Result<(), ServiceError> {
        self.reserve_range_as(base, size, E820Kind::Reserved)
    }

    /// Carve a range with a specific e820 kind out of RAM entries in-place.
    /// An oversized result leaves the original map intact rather than losing entries.
    pub fn reserve_range_as(
        &mut self,
        base: u64,
        size: u64,
        kind: E820Kind,
    ) -> Result<(), ServiceError> {
        if size == 0 {
            return Ok(());
        }
        let end = base.checked_add(size).ok_or(ServiceError::InvalidParam)?;

        let mut out = [E820Entry::zeroed(); MAX_E820_ENTRIES];
        let mut out_count = 0usize;
        for entry in self.entries().iter().copied() {
            let entry_end = entry.addr.saturating_add(entry.size);
            if entry.kind != E820Kind::Ram as u32 || end <= entry.addr || base >= entry_end {
                append_entry(&mut out, &mut out_count, entry)?;
                continue;
            }

            if entry.addr < base {
                append_entry(
                    &mut out,
                    &mut out_count,
                    E820Entry::new(entry.addr, base - entry.addr, E820Kind::Ram),
                )?;
            }

            let res_base = entry.addr.max(base);
            let res_end = entry_end.min(end);
            if res_end > res_base {
                append_entry(
                    &mut out,
                    &mut out_count,
                    E820Entry::new(res_base, res_end - res_base, kind),
                )?;
            }

            if entry_end > end {
                append_entry(
                    &mut out,
                    &mut out_count,
                    E820Entry::new(end, entry_end - end, E820Kind::Ram),
                )?;
            }
        }

        self.entries = out;
        self.count = out_count;
        Ok(())
    }
}

fn append_entry(
    entries: &mut [E820Entry; MAX_E820_ENTRIES],
    count: &mut usize,
    entry: E820Entry,
) -> Result<(), ServiceError> {
    if entry.size == 0 {
        return Ok(());
    }
    if *count > 0 {
        let prev = &mut entries[*count - 1];
        if prev.kind == entry.kind && prev.addr.saturating_add(prev.size) == entry.addr {
            prev.size = prev.size.saturating_add(entry.size);
            return Ok(());
        }
    }
    *entries.get_mut(*count).ok_or(ServiceError::HardwareError)? = entry;
    *count += 1;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserving_with_a_full_map_fails_without_truncating() {
        let mut map = E820State::new();
        for (idx, entry) in map.entries_mut().iter_mut().enumerate() {
            *entry = if idx == MAX_E820_ENTRIES - 1 {
                E820Entry::new(0x10_0000, 0x10_000, E820Kind::Ram)
            } else {
                E820Entry::new((idx as u64) * 0x2000, 0x1000, E820Kind::Reserved)
            };
        }
        assert_eq!(
            map.set_detected(MAX_E820_ENTRIES + 1, 0),
            Err(ServiceError::InvalidParam)
        );
        map.set_detected(MAX_E820_ENTRIES, 0).unwrap();
        assert_eq!(
            map.reserve_range(0x10_2000, 0x1000),
            Err(ServiceError::HardwareError)
        );
        assert_eq!(map.count(), MAX_E820_ENTRIES);
        let last = map.entries()[MAX_E820_ENTRIES - 1];
        let addr = last.addr;
        let size = last.size;
        assert_eq!(addr, 0x10_0000);
        assert_eq!(size, 0x10_000);
    }

    #[test]
    fn reservation_splits_ram_without_losing_neighbors() {
        let mut map = E820State::new();
        map.entries_mut()[0] = E820Entry::new(0x1000, 0x4000, E820Kind::Ram);
        map.set_detected(1, 0x4000).unwrap();
        map.reserve_range_as(0x2000, 0x1000, E820Kind::Nvs).unwrap();
        assert_eq!(map.count(), 3);
        let kinds = [
            map.entries()[0].kind,
            map.entries()[1].kind,
            map.entries()[2].kind,
        ];
        assert_eq!(
            kinds,
            [
                E820Kind::Ram as u32,
                E820Kind::Nvs as u32,
                E820Kind::Ram as u32
            ]
        );
    }
}

// No global e820 state: the mainstage context owns the authoritative
// E820State and passes references to consumers. A global static would sit
// in every stage's .bss — including bootblocks with tiny CAR windows — and
// invited divergence between the global and per-stage copies.

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
