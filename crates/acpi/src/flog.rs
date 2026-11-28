//! `FLOG`: a standard ACPI SDT followed by the retained firmware console bytes
//! in chronological order. The reserved slot stays put while SDT length and
//! checksum describe only the current snapshot (including before OS handoff).
//! On Linux, `tail -c +37 /sys/firmware/acpi/tables/FLOG` skips the SDT header;
//! no platform driver is required. A wrapped ring can start mid-line.

use acpi_tables::{Checksum, sdt::Sdt};
use alloc::vec::Vec;
use fstart_core::description::Range;
use fstart_log::mem;

pub const SIGNATURE: [u8; 4] = *b"FLOG";
const HEADER_SIZE: usize = 36;

/// Build directly in the reserved table body. No table is needed on a stage
/// with no attached memory console. Ring capacity is bounded to 31 bits.
pub fn build_reserved() -> Option<Vec<u8>> {
    let cap = mem::capacity();
    if cap == 0 {
        return None;
    }
    let header = Sdt::new(
        SIGNATURE,
        HEADER_SIZE as u32,
        1,
        crate::OEM_ID,
        crate::OEM_TABLE_ID,
        crate::OEM_REVISION,
    );
    let mut table = Vec::with_capacity(HEADER_SIZE + cap);
    table.extend_from_slice(header.as_slice());
    table.resize(HEADER_SIZE + cap, 0);
    update(&mut table, Snapshot::Continue);
    Some(table)
}

/// Which handoff boundary is taking the snapshot.
#[derive(Clone, Copy)]
pub enum Snapshot {
    /// More console output can follow (UEFI DMA quiescence).
    Continue,
    /// Stop writing to the ring atomically with the copy.
    Final,
}

/// Refresh the pre-reserved table without allocating at OS handoff.
///
/// # Safety
/// `slot` is the entire writable, identity-mapped FLOG reservation returned by
/// table placement. It must be disjoint from the ring and exclusively accessible
/// during this call.
pub unsafe fn refresh(slot: Range, snapshot: Snapshot) {
    // SAFETY: the platform retains this exclusive store reservation until handoff.
    let table =
        unsafe { core::slice::from_raw_parts_mut(slot.base as *mut u8, slot.size as usize) };
    update(table, snapshot);
}

fn update(table: &mut [u8], snapshot: Snapshot) {
    let Some((header, body)) = table
        .split_at_mut_checked(HEADER_SIZE)
        .filter(|(header, _)| header[..4] == SIGNATURE)
    else {
        // Even a damaged table must not leave the sink live after handoff.
        if matches!(snapshot, Snapshot::Final) {
            mem::detach();
        }
        return;
    };
    let used = match snapshot {
        Snapshot::Continue => mem::copy_to(body),
        Snapshot::Final => mem::copy_to_and_detach(body),
    };
    body[used..].fill(0);
    header[4..8].copy_from_slice(&((HEADER_SIZE + used) as u32).to_le_bytes());
    header[9] = 0;
    let mut checksum = Checksum::default();
    checksum.append(header);
    checksum.append(&body[..used]);
    header[9] = checksum.value();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(table: &[u8], expected: &[u8]) {
        let len = u32::from_le_bytes(table[4..8].try_into().unwrap()) as usize;
        assert_eq!(&table[..4], &SIGNATURE);
        assert_eq!(&table[HEADER_SIZE..len], expected);
        assert_eq!(table[..len].iter().copied().fold(0u8, u8::wrapping_add), 0);
        assert!(table[len..].iter().all(|&b| b == 0));
    }

    #[test]
    fn handoff_updates_only_the_reserved_slot_and_detaches_on_final_snapshot() {
        let mut backing = [0u32; 16];
        // SAFETY: aligned backing exclusively owned until the final snapshot.
        assert!(unsafe { mem::init(backing.as_mut_ptr().cast(), size_of_val(&backing)) });
        mem::write_bytes(b"boot");
        let mut table = build_reserved().unwrap();
        check(&table, b"boot");
        let slot = Range::new(table.as_mut_ptr() as u64, table.len() as u64);
        mem::write_bytes(b" UEFI");
        // SAFETY: `table` exclusively owns the complete reservation.
        unsafe { refresh(slot, Snapshot::Continue) };
        check(&table, b"boot UEFI");
        mem::write_bytes(b" exit");
        unsafe { refresh(slot, Snapshot::Final) };
        check(&table, b"boot UEFI exit");
        assert!(!mem::is_attached());
        assert!(build_reserved().is_none());
    }
}
