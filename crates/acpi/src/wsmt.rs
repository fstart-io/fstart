//! WSMT (Windows SMM Security Mitigations Table, Microsoft WSMT 1.0): the
//! protections the platform's SMM handlers guarantee.

use acpi_tables::sdt::Sdt;
use alloc::vec::Vec;
use zerocopy::{Immutable, IntoBytes, little_endian::U32};

#[derive(IntoBytes, Immutable)]
#[repr(C)]
struct WsmtBody {
    protection_flags: U32,
}

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
        36 + core::mem::size_of::<WsmtBody>() as u32,
        1,
        crate::OEM_ID,
        crate::OEM_TABLE_ID,
        crate::OEM_REVISION,
    );
    sdt.write_bytes(
        36,
        WsmtBody {
            protection_flags: U32::new(protections),
        }
        .as_bytes(),
    );
    crate::serialize(&sdt)
}
