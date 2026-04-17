//! Sophgo gen_spi_flash disk partition table (DPT) parser.
//!
//! Parses the partition table written by Sophgo's `gen_spi_flash` tool,
//! located at flash offset `0x600000`. Used by the RISC-V release driver
//! to find the ZSBL binary partition (load address and size).
//!
//! # Partition table format
//!
//! Entries are contiguous 52-byte structs starting at the DPT base
//! address. The table ends at the first entry whose `magic` field is
//! not `DPT_MAGIC`. Names are null-terminated ASCII up to 32 bytes.
//!
//! Hardware reference: `mango_misc.c` — `struct part_info`, `DPT_MAGIC`,
//! `DISK_PART_TABLE_ADDR = 0x600000`.

/// Magic value that must appear in every valid partition table entry.
pub const DPT_MAGIC: u32 = 0x55aa_55aa;

/// A single partition table entry.
///
/// Layout is identical to the C `struct part_info` / `sf_part_info`.
/// Total size = 52 bytes (verified by compile-time assertion).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DptEntry {
    /// Must equal [`DPT_MAGIC`] for a valid entry.
    pub magic: u32,
    /// Null-terminated partition name (e.g., "zsbl", "fw_dynamic.bin").
    pub name: [u8; 32],
    /// Byte offset of the partition within SPI flash.
    pub offset: u32,
    /// Partition size in bytes.
    pub size: u32,
    /// Reserved (padding to 52 bytes).
    pub _reserve: [u8; 4],
    /// Load memory address — where to copy this partition in DRAM.
    pub lma: u64,
}

// DptEntry layout on a 64-bit target (matching C `struct part_info`):
// magic(4) + name(32) + offset(4) + size(4) + _reserve(4) + [4-byte pad] + lma(8) = 56
// The 4-byte pad is inserted by the C compiler to align lma to 8 bytes.
const _: () = assert!(core::mem::size_of::<DptEntry>() == 56);

impl DptEntry {
    /// Return the partition name as a byte slice, stopping at the first
    /// null byte (or the full 32 bytes if no null is present).
    pub fn name_bytes(&self) -> &[u8] {
        let end = self.name.iter().position(|&b| b == 0).unwrap_or(32);
        &self.name[..end]
    }
}

/// Walk the DPT at `table_base` and return a pointer to the entry whose
/// name matches `name`, or `None` if not found.
///
/// # Safety
///
/// `table_base` must point to a valid, readable SPI flash DMMR window
/// (e.g., `SERIAL_FLASH0_BASE + DPT_OFFSET`). Entries are read until the
/// first entry with `magic != DPT_MAGIC`. The caller guarantees the
/// memory region is mapped and accessible.
///
/// The returned pointer is valid for the lifetime of the DMMR mapping.
pub unsafe fn find_by_name(table_base: *const u8, name: &[u8]) -> Option<*const DptEntry> {
    let mut ptr = table_base as *const DptEntry;
    loop {
        // SAFETY: caller guarantees the DMMR window is mapped and large
        // enough to contain a full DPT. We read the magic field first to
        // detect the end-of-table sentinel before accessing other fields.
        let magic = core::ptr::read_volatile(core::ptr::addr_of!((*ptr).magic));
        if magic != DPT_MAGIC {
            return None;
        }
        let entry = &*ptr;
        if entry.name_bytes() == name {
            return Some(ptr);
        }
        // SAFETY: pointer arithmetic within the DPT window.
        ptr = ptr.offset(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(name: &[u8], offset: u32, size: u32, lma: u64) -> DptEntry {
        let mut entry = DptEntry {
            magic: DPT_MAGIC,
            name: [0u8; 32],
            offset,
            size,
            _reserve: [0u8; 4],
            lma,
        };
        let copy_len = name.len().min(32);
        entry.name[..copy_len].copy_from_slice(&name[..copy_len]);
        entry
    }

    #[test]
    fn test_dpt_entry_size() {
        // 4+32+4+4+4 = 48 bytes before lma, which is 8-byte aligned (48%8==0).
        // lma(u64) = 8 bytes. Total = 56 on 64-bit targets.
        assert_eq!(core::mem::size_of::<DptEntry>(), 56);
    }

    #[test]
    fn test_find_by_name_first_entry() {
        let table = [make_entry(b"zsbl", 0x1000, 0x8000, 0x8000_0000)];
        let result = unsafe { find_by_name(table.as_ptr() as *const u8, b"zsbl") };
        assert!(result.is_some());
        let entry = unsafe { &*result.unwrap() };
        assert_eq!(entry.offset, 0x1000);
        assert_eq!(entry.lma, 0x8000_0000);
    }

    #[test]
    fn test_find_by_name_second_entry() {
        let table = [
            make_entry(b"fw_dynamic.bin", 0x2000, 0x4000, 0x8010_0000),
            make_entry(b"zsbl", 0x1000, 0x8000, 0x8000_0000),
        ];
        let result = unsafe { find_by_name(table.as_ptr() as *const u8, b"zsbl") };
        assert!(result.is_some());
        let entry = unsafe { &*result.unwrap() };
        assert_eq!(entry.offset, 0x1000);
    }

    #[test]
    fn test_find_by_name_bad_magic_returns_none() {
        let mut table = [make_entry(b"zsbl", 0x1000, 0x8000, 0x0)];
        table[0].magic = 0xDEAD_BEEF; // corrupt magic
        let result = unsafe { find_by_name(table.as_ptr() as *const u8, b"zsbl") };
        assert!(result.is_none());
    }

    #[test]
    fn test_find_by_name_not_found_returns_none() {
        let table = [
            make_entry(b"fw_dynamic.bin", 0x2000, 0x4000, 0x0),
            // Sentinel: invalid magic stops the walk
            DptEntry {
                magic: 0,
                name: [0u8; 32],
                offset: 0,
                size: 0,
                _reserve: [0u8; 4],
                lma: 0,
            },
        ];
        let result = unsafe { find_by_name(table.as_ptr() as *const u8, b"zsbl") };
        assert!(result.is_none());
    }

    #[test]
    fn test_name_bytes_stops_at_null() {
        let entry = make_entry(b"zsbl", 0, 0, 0);
        assert_eq!(entry.name_bytes(), b"zsbl");
    }

    #[test]
    fn test_name_bytes_full_32_no_null() {
        let mut entry = make_entry(b"", 0, 0, 0);
        entry.name = [b'x'; 32];
        assert_eq!(entry.name_bytes().len(), 32);
    }
}
