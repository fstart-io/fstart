//! ACPI S3 wake-vector discovery.
//!
//! The S3 path must use the OS-owned ACPI tables left in memory, not newly
//! generated firmware tables. This module scans the legacy BIOS area for the
//! old RSDP, walks RSDT/XSDT to FADT, then reads the FACS firmware waking
//! vector.

/// Legacy BIOS area scanned by coreboot for the OS RSDP on S3 resume.
pub const LEGACY_RSDP_SCAN_START: usize = 0x000e_0000;
/// End of the legacy RSDP scan range, exclusive.
pub const LEGACY_RSDP_SCAN_END: usize = 0x0010_0000;
const RSDP_SIGNATURE: &[u8; 8] = b"RSD PTR ";
const FADT_SIGNATURE: &[u8; 4] = b"FACP";

/// Locate an ACPI S3 firmware waking vector from preserved OS tables.
///
/// # Safety
///
/// The caller must ensure the legacy BIOS area and ACPI physical addresses are
/// identity-mapped and readable. This is intended for firmware running before
/// paging changes on x86 resume.
pub unsafe fn find_wakeup_vector_legacy() -> Option<u64> {
    let mut addr = LEGACY_RSDP_SCAN_START;
    while addr < LEGACY_RSDP_SCAN_END {
        if let Some(vector) = unsafe { find_wakeup_vector_from_rsdp(addr) } {
            return Some(vector);
        }
        addr += 16;
    }
    None
}

/// Locate an ACPI S3 firmware waking vector from a candidate RSDP address.
///
/// # Safety
///
/// `rsdp_addr` and table pointers discovered from it must be readable physical
/// memory mappings.
pub unsafe fn find_wakeup_vector_from_rsdp(rsdp_addr: usize) -> Option<u64> {
    let rsdp = unsafe { core::slice::from_raw_parts(rsdp_addr as *const u8, 36) };
    if &rsdp[0..8] != RSDP_SIGNATURE || checksum(&rsdp[..20]) != 0 {
        return None;
    }

    let revision = rsdp[15];
    let rsdt_addr = u32::from_le_bytes(rsdp[16..20].try_into().ok()?) as usize;

    if revision >= 2 && checksum(rsdp) == 0 {
        let xsdt_addr = u64::from_le_bytes(rsdp[24..32].try_into().ok()?) as usize;
        if xsdt_addr != 0 {
            if let Some(fadt) = unsafe { find_table(xsdt_addr, true, FADT_SIGNATURE) } {
                return unsafe { wake_vector_from_fadt(fadt) };
            }
        }
    }

    if rsdt_addr != 0 {
        if let Some(fadt) = unsafe { find_table(rsdt_addr, false, FADT_SIGNATURE) } {
            return unsafe { wake_vector_from_fadt(fadt) };
        }
    }
    None
}

unsafe fn find_table(root_addr: usize, xsdt: bool, signature: &[u8; 4]) -> Option<usize> {
    let header = unsafe { core::slice::from_raw_parts(root_addr as *const u8, 36) };
    checksum_table(root_addr)?;
    let len = u32::from_le_bytes(header[4..8].try_into().ok()?) as usize;
    if len < 36 {
        return None;
    }
    let entry_size = if xsdt { 8 } else { 4 };
    let entries = (len - 36) / entry_size;
    let table = unsafe { core::slice::from_raw_parts(root_addr as *const u8, len) };
    for i in 0..entries {
        let off = 36 + i * entry_size;
        let addr = if xsdt {
            u64::from_le_bytes(table[off..off + 8].try_into().ok()?) as usize
        } else {
            u32::from_le_bytes(table[off..off + 4].try_into().ok()?) as usize
        };
        if addr == 0 {
            continue;
        }
        let sig = unsafe { core::slice::from_raw_parts(addr as *const u8, 4) };
        if sig == signature && checksum_table(addr).is_some() {
            return Some(addr);
        }
    }
    None
}

unsafe fn wake_vector_from_fadt(fadt_addr: usize) -> Option<u64> {
    let fadt = unsafe { core::slice::from_raw_parts(fadt_addr as *const u8, 148) };
    let len = u32::from_le_bytes(fadt[4..8].try_into().ok()?) as usize;
    if len < 116 {
        return None;
    }

    // FADT Firmware Control field at offset 36, X_Firmware_Control at 132.
    let facs32 = u32::from_le_bytes(fadt[36..40].try_into().ok()?) as u64;
    let facs = if len >= 140 {
        let facs64 = u64::from_le_bytes(fadt[132..140].try_into().ok()?);
        if facs64 != 0 {
            facs64
        } else {
            facs32
        }
    } else {
        facs32
    };
    if facs == 0 {
        return None;
    }

    let facs_bytes = unsafe { core::slice::from_raw_parts(facs as usize as *const u8, 32) };
    if &facs_bytes[0..4] != b"FACS" {
        return None;
    }
    // FACS FirmwareWakingVector offset 12; XFirmwareWakingVector offset 24.
    let vector32 = u32::from_le_bytes(facs_bytes[12..16].try_into().ok()?) as u64;
    let vector64 = u64::from_le_bytes(facs_bytes[24..32].try_into().ok()?);
    match (vector64, vector32) {
        (v, _) if v != 0 => Some(v),
        (_, v) if v != 0 => Some(v),
        _ => None,
    }
}

fn checksum_table(addr: usize) -> Option<()> {
    // SAFETY: caller arranged that ACPI physical tables are readable.
    let header = unsafe { core::slice::from_raw_parts(addr as *const u8, 36) };
    let len = u32::from_le_bytes(header[4..8].try_into().ok()?) as usize;
    if len < 36 {
        return None;
    }
    let bytes = unsafe { core::slice::from_raw_parts(addr as *const u8, len) };
    (checksum(bytes) == 0).then_some(())
}

fn checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte))
}
