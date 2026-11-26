//! WDAT (Watchdog Action Table, Microsoft WDAT 1.0): the actions through
//! which the OS drives a hardware watchdog, each a register access.

use acpi_tables::sdt::Sdt;
use alloc::vec::Vec;

/// Instruction flag: preserve the bits outside the mask on a write.
pub const PRESERVE_REGISTER: u8 = 0x80;

/// One watchdog instruction entry: `action`, `instruction` (with
/// [`PRESERVE_REGISTER`]), a 32-bit system-memory register at `address`,
/// `value` and `mask`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub action: u8,
    pub instruction: u8,
    pub address: u64,
    pub value: u32,
    pub mask: u32,
}

/// The watchdog's parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Watchdog {
    /// Milliseconds per count.
    pub period_ms: u32,
    pub max_count: u32,
    pub min_count: u32,
    /// Bit 0 enabled, bit 7 stopped in sleeping states.
    pub flags: u8,
}

/// The WDAT of a watchdog with no PCI function (`FFh` everywhere).
pub fn build_wdat(
    oem_id: [u8; 6],
    oem_table_id: [u8; 8],
    oem_revision: u32,
    watchdog: &Watchdog,
    entries: &[Entry],
) -> Vec<u8> {
    let mut sdt = Sdt::new(*b"WDAT", 68, 1, oem_id, oem_table_id, oem_revision);
    sdt.write_u32(36, 32);
    sdt.write_u16(40, 0xff);
    sdt.write_u8(42, 0xff);
    sdt.write_u8(43, 0xff);
    sdt.write_u8(44, 0xff);
    sdt.write_u32(48, watchdog.period_ms);
    sdt.write_u32(52, watchdog.max_count);
    sdt.write_u32(56, watchdog.min_count);
    sdt.write_u8(60, watchdog.flags);
    sdt.write_u32(64, entries.len() as u32);
    for e in entries {
        let mut bytes = [0u8; 24];
        bytes[0] = e.action;
        bytes[1] = e.instruction;
        // System memory, 32 bits at bit 0, dword access.
        bytes[4..8].copy_from_slice(&[0, 32, 0, 3]);
        bytes[8..16].copy_from_slice(&e.address.to_le_bytes());
        bytes[16..20].copy_from_slice(&e.value.to_le_bytes());
        bytes[20..24].copy_from_slice(&e.mask.to_le_bytes());
        sdt.append_slice(&bytes);
    }
    sdt.update_checksum();
    crate::serialize(&sdt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_and_entries_where_the_spec_puts_them() {
        let t = build_wdat(
            *b"FSTART",
            *b"FSTARTFW",
            1,
            &Watchdog {
                period_ms: 1000,
                max_count: 0x3ff,
                min_count: 2,
                flags: 0x81,
            },
            &[Entry {
                action: 1,
                instruction: 0x82,
                address: 0xfed8_0b00,
                value: 0x80,
                mask: 0x80,
            }],
        );
        assert_eq!(&t[..4], b"WDAT");
        assert_eq!(t.len(), 68 + 24);
        assert_eq!(&t[40..45], &[0xff, 0, 0xff, 0xff, 0xff]);
        assert_eq!(u32::from_le_bytes(t[48..52].try_into().unwrap()), 1000);
        assert_eq!(t[60], 0x81);
        assert_eq!(&t[68..76], &[1, 0x82, 0, 0, 0, 32, 0, 3]);
        assert_eq!(&t[76..84], &0xfed8_0b00u64.to_le_bytes());
        assert_eq!(t.iter().fold(0u8, |a, b| a.wrapping_add(*b)), 0);
    }
}
