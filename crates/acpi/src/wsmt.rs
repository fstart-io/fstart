//! WSMT (Windows SMM Security Mitigations Table, Microsoft WSMT 1.0): the
//! protections the platform's SMM handlers guarantee.

use acpi_tables::sdt::Sdt;
use alloc::vec::Vec;

/// SMM uses only fixed communication buffers it validates.
pub const FIXED_COMM_BUFFERS: u32 = 1 << 0;
/// SMM follows no pointer taken from a communication buffer without
/// validating it.
pub const COMM_BUFFER_NESTED_PTR_PROTECTION: u32 = 1 << 1;
/// Critical system resources are locked against change through SMM.
pub const SYSTEM_RESOURCE_PROTECTION: u32 = 1 << 2;

/// Build the WSMT (revision 1) with `protections`.
pub fn build_wsmt(protections: u32) -> Vec<u8> {
    let mut sdt = Sdt::new(
        *b"WSMT",
        40,
        1,
        crate::OEM_ID,
        crate::OEM_TABLE_ID,
        crate::OEM_REVISION,
    );
    sdt.write_u32(36, protections);
    crate::serialize(&sdt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout() {
        let t = build_wsmt(FIXED_COMM_BUFFERS | COMM_BUFFER_NESTED_PTR_PROTECTION);
        assert_eq!(t.len(), 40);
        assert_eq!(&t[..4], b"WSMT");
        assert_eq!(&t[36..40], &3u32.to_le_bytes());
        assert_eq!(t.iter().fold(0u8, |a, b| a.wrapping_add(*b)), 0);
    }
}
