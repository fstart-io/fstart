//! ACPI `FLOG` (Firmware Log) table builder.
//!
//! Stores the firmware's in-memory boot log directly after a standard 36-byte
//! ACPI System Description Table header so any OS exposes it without a custom
//! driver (for example `/sys/firmware/acpi/tables/FLOG` on Linux).
//!
//! Because later firmware phases (such as CrabEFI) keep logging after ACPI
//! tables are initially assembled, [`build_reserved`] reserves a fixed-capacity
//! slot inside the ACPI table area that [`fstart_log::mem::finalize_acpi_table`]
//! patches in place right before OS handoff.

use alloc::vec;
use alloc::vec::Vec;

/// 4-byte ACPI signature for the firmware log table.
pub const SIGNATURE: [u8; 4] = *b"FLOG";

/// Table revision.
pub const REVISION: u8 = 1;

/// Standard ACPI SDT header size in bytes.
pub const HEADER_SIZE: usize = 36;

const RSDP_XSDT_OFFSET: usize = 24;
const XSDT_ENTRY_SIZE: usize = 8;

/// Build a `FLOG` table with `capacity` bytes reserved for the log body and
/// populated with `initial` bytes (keeping the tail if `initial` exceeds
/// `capacity`).
///
/// The returned `Vec` has length `HEADER_SIZE + capacity` so the ACPI assembler
/// reserves the full slot, while the SDT `Length` and `Checksum` fields cover
/// `HEADER_SIZE + body_len` so the table is already valid even if no later
/// patch runs.
#[must_use]
pub fn build_reserved(capacity: usize, initial: &[u8]) -> Vec<u8> {
    let body = if initial.len() > capacity {
        &initial[initial.len() - capacity..]
    } else {
        initial
    };
    let mut table = vec![0u8; HEADER_SIZE + capacity];
    let active_len = HEADER_SIZE + body.len();
    table[0..4].copy_from_slice(&SIGNATURE);
    table[4..8].copy_from_slice(&(active_len as u32).to_le_bytes());
    table[8] = REVISION;
    table[10..16].copy_from_slice(&crate::OEM_ID);
    table[16..24].copy_from_slice(&crate::OEM_TABLE_ID);
    table[24..28].copy_from_slice(&crate::OEM_REVISION.to_le_bytes());
    table[28..32].copy_from_slice(&acpi_tables::CREATOR_ID);
    table[32..36].copy_from_slice(&acpi_tables::CREATOR_REVISION);
    table[HEADER_SIZE..active_len].copy_from_slice(body);
    let sum = table[..active_len]
        .iter()
        .fold(0u8, |acc, &b| acc.wrapping_add(b));
    table[9] = 0u8.wrapping_sub(sum);
    table
}

/// Locate the `FLOG` table's physical address inside a contiguous ACPI buffer
/// assembled at `base_addr` (starting with RSDP), verifying that the entire
/// `expected_slot_len` bytes lie within `assembled`.
#[must_use]
pub fn find_in_assembled(
    assembled: &[u8],
    base_addr: u64,
    expected_slot_len: usize,
) -> Option<u64> {
    if expected_slot_len < HEADER_SIZE {
        return None;
    }
    let xsdt_bytes = assembled.get(RSDP_XSDT_OFFSET..RSDP_XSDT_OFFSET + 8)?;
    let xsdt_addr = u64::from_le_bytes(xsdt_bytes.try_into().ok()?);
    let xsdt_off = usize::try_from(xsdt_addr.checked_sub(base_addr)?).ok()?;
    let xsdt_len_start = xsdt_off.checked_add(4)?;
    let xsdt_len_end = xsdt_off.checked_add(8)?;
    let xsdt_len_bytes = assembled.get(xsdt_len_start..xsdt_len_end)?;
    let xsdt_len = u32::from_le_bytes(xsdt_len_bytes.try_into().ok()?) as usize;
    if xsdt_len < HEADER_SIZE || xsdt_off.checked_add(xsdt_len)? > assembled.len() {
        return None;
    }
    let entries = (xsdt_len - HEADER_SIZE) / XSDT_ENTRY_SIZE;
    for i in 0..entries {
        let off = xsdt_off
            .checked_add(HEADER_SIZE)?
            .checked_add(i.checked_mul(XSDT_ENTRY_SIZE)?)?;
        let end = off.checked_add(8)?;
        let addr = u64::from_le_bytes(assembled.get(off..end)?.try_into().ok()?);
        let table_off = usize::try_from(addr.checked_sub(base_addr)?).ok()?;
        let slot_end = table_off.checked_add(expected_slot_len)?;
        if slot_end <= assembled.len()
            && assembled.get(table_off..table_off + 4) == Some(&SIGNATURE)
        {
            return Some(addr);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{FadtConfig, assemble};

    #[test]
    fn build_reserved_produces_valid_sdt_and_is_found_in_assembled() {
        let flog = build_reserved(128, b"[INFO ] hello\n");
        assert_eq!(flog.len(), HEADER_SIZE + 128);
        assert_eq!(&flog[0..4], &SIGNATURE);
        let active_len = u32::from_le_bytes(flog[4..8].try_into().unwrap()) as usize;
        assert_eq!(active_len, HEADER_SIZE + 14);
        assert_eq!(&flog[HEADER_SIZE..active_len], b"[INFO ] hello\n");
        let sum = flog[..active_len]
            .iter()
            .fold(0u8, |acc, &b| acc.wrapping_add(b));
        assert_eq!(sum, 0);

        let base = 0x1000_0000u64;
        let reserved = flog.len();
        let assembled = assemble(base, &FadtConfig::default(), &[], &[], &[flog]);
        let found = find_in_assembled(&assembled, base, reserved).expect("FLOG in XSDT");
        assert!(find_in_assembled(&assembled, base, assembled.len() + 1).is_none());
        let off = (found - base) as usize;
        assert_eq!(&assembled[off..off + 4], &SIGNATURE);
        assert_eq!(
            &assembled[off + HEADER_SIZE..off + HEADER_SIZE + 14],
            b"[INFO ] hello\n"
        );
    }

    #[test]
    fn build_reserved_keeps_tail_when_initial_exceeds_capacity() {
        let flog = build_reserved(4, b"abcdef");
        assert_eq!(flog.len(), HEADER_SIZE + 4);
        let active_len = u32::from_le_bytes(flog[4..8].try_into().unwrap()) as usize;
        assert_eq!(active_len, HEADER_SIZE + 4);
        assert_eq!(&flog[HEADER_SIZE..active_len], b"cdef");
        let sum = flog[..active_len]
            .iter()
            .fold(0u8, |acc, &b| acc.wrapping_add(b));
        assert_eq!(sum, 0);
    }
}
