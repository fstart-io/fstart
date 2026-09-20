//! Architecture-neutral ACPI wake-vector lookup.
//!
//! Given the physical address of an RSDP and a caller-provided physical-memory
//! reader, this module follows RSDP -> XSDT -> FADT -> FACS and returns the
//! firmware waking vector saved by the operating system. Locating the RSDP is
//! platform-specific and belongs in the architecture platform module.

const MAX_XSDT_LEN: usize = 4096;
const RSDP_SIG: &[u8; 8] = b"RSD PTR ";
const RSDP_LEN: usize = 36;
const SDT_LEN_OFF: usize = 4;
const RSDP_XSDT_OFF: usize = 24;
const XSDT_ENTRY_OFF: usize = 36;
const FADT_LEN: usize = 148;
const FADT_FIRMWARE_CTRL_OFF: usize = 36;
const FADT_X_FIRMWARE_CTL_OFF: usize = 132;
const FACS_LEN: usize = 16;
const FACS_WAKE_VECTOR_OFF: usize = 12;

fn u32_at(bytes: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(off..off + 4)?.try_into().ok()?))
}

fn u64_at(bytes: &[u8], off: usize) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.get(off..off + 8)?.try_into().ok()?))
}

fn checksum_ok(bytes: &[u8]) -> bool {
    bytes.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)) == 0
}

fn rsdp_xsdt_addr(bytes: &[u8]) -> Option<u64> {
    if bytes.len() < RSDP_LEN || &bytes[..8] != RSDP_SIG || !checksum_ok(&bytes[..20]) {
        return None;
    }
    if bytes[15] >= 2 {
        let len = u32_at(bytes, 20)? as usize;
        if len < RSDP_LEN || bytes.len() < len || !checksum_ok(&bytes[..len]) {
            return None;
        }
    }
    u64_at(bytes, RSDP_XSDT_OFF).filter(|address| *address != 0)
}

/// Validate the RSDP at `rsdp_addr` without walking the rest of the table set.
#[must_use]
pub fn rsdp_is_valid_with(read: &impl Fn(u64, &mut [u8]), rsdp_addr: u64) -> bool {
    let mut rsdp = [0u8; RSDP_LEN];
    read(rsdp_addr, &mut rsdp);
    rsdp_xsdt_addr(&rsdp).is_some()
}

fn find_fadt_in_xsdt(read: &impl Fn(u64, &mut [u8]), xsdt_addr: u64) -> Option<u64> {
    let mut header = [0u8; XSDT_ENTRY_OFF];
    read(xsdt_addr, &mut header);
    if &header[..4] != b"XSDT" {
        return None;
    }
    let len = (u32_at(&header, SDT_LEN_OFF)? as usize).clamp(XSDT_ENTRY_OFF, MAX_XSDT_LEN);
    let mut bytes = [0u8; MAX_XSDT_LEN];
    read(xsdt_addr, &mut bytes[..len]);
    let entries = (len - XSDT_ENTRY_OFF) / 8;
    (0..entries)
        .filter_map(|index| u64_at(&bytes, XSDT_ENTRY_OFF + index * 8))
        .filter(|address| *address != 0)
        .find(|address| {
            let mut signature = [0u8; 4];
            read(*address, &mut signature);
            &signature == b"FACP"
        })
}

fn fadt_facs_addr(bytes: &[u8]) -> Option<u64> {
    if bytes.len() < FADT_LEN || &bytes[..4] != b"FACP" {
        return None;
    }
    match u64_at(bytes, FADT_X_FIRMWARE_CTL_OFF)? {
        0 => u32_at(bytes, FADT_FIRMWARE_CTRL_OFF)
            .map(u64::from)
            .filter(|address| *address != 0),
        address => Some(address),
    }
}

fn facs_wake_vector(bytes: &[u8]) -> Option<u32> {
    if bytes.len() < FACS_LEN || &bytes[..4] != b"FACS" {
        return None;
    }
    u32_at(bytes, FACS_WAKE_VECTOR_OFF).filter(|vector| *vector != 0)
}

/// Follow an ACPI table chain starting at `rsdp_addr` to the OS wake vector.
///
/// `read(addr, buf)` copies physical memory into `buf`. Every externally sized
/// table is bounded before it is read.
#[must_use]
pub fn wakeup_vector_from_rsdp_with(
    read: &impl Fn(u64, &mut [u8]),
    rsdp_addr: u64,
) -> Option<u32> {
    let mut rsdp = [0u8; RSDP_LEN];
    read(rsdp_addr, &mut rsdp);
    let xsdt_addr = rsdp_xsdt_addr(&rsdp)?;
    let fadt_addr = find_fadt_in_xsdt(read, xsdt_addr)?;

    let mut fadt = [0u8; FADT_LEN];
    read(fadt_addr, &mut fadt);
    let facs_addr = fadt_facs_addr(&fadt)?;

    let mut facs = [0u8; FACS_LEN];
    read(facs_addr, &mut facs);
    facs_wake_vector(&facs)
}
