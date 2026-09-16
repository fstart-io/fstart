//! Locate the OS wake vector for S3 resume.
//!
//! Per ACPI, an OS writes its real-mode resume address into
//! `FACS.firmware_waking_vector` before entering S3. The tables live in
//! reserved RAM and survive S3, so on wake the firmware can find the vector
//! by walking the *surviving* low-memory RSDP, exactly like coreboot's
//! `acpi_find_wakeup_vector()`. This must run before the table set is
//! re-emitted, since rebuilding zeroes the FACS.
//!
//! The parsers are pure byte decoders (no struct casts) driven by a caller
//! supplied physical-memory reader, so the whole walk is host-testable.
//! The firmware entry point reads through the x86 identity map.

/// BDA pointer to the EBDA segment (physical `0x40E`).
const BDA_EBDA_SEG_PTR: u64 = 0x40E;
/// Legacy RSDP scan window (top of conventional memory).
const SCAN_LO: u64 = 0xE_0000;
const SCAN_HI: u64 = 0xF_FFFF;
/// Bound on XSDT bytes consumed from untrusted (S3-resident) memory.
const MAX_XSDT_LEN: usize = 4096;
/// Wake vectors are 16-bit real-mode code, hence below 1 MiB.
const REAL_MODE_LIMIT: u32 = 0x10_0000;

const RSDP_SIG: &[u8; 8] = b"RSD PTR ";
const RSDP_LEN: usize = 36;
const SDT_LEN_OFF: usize = 4;
const RSDP_XSDT_OFF: usize = 24;
const XSDT_ENTRY_OFF: usize = 36;
const FADT_LEN: usize = 148;
/// ACPI 1.0 32-bit FACS pointer.
const FADT_FIRMWARE_CTRL_OFF: usize = 36;
/// ACPI 2.0+ 64-bit FACS pointer (verified against the D41S FADT dump).
const FADT_X_FIRMWARE_CTL_OFF: usize = 132;
const FACS_LEN: usize = 16;
const FACS_WAKE_VECTOR_OFF: usize = 12;

fn u32_at(bytes: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap_or([0; 4]))
}

fn u64_at(bytes: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(bytes[off..off + 8].try_into().unwrap_or([0; 8]))
}

fn checksum_ok(bytes: &[u8]) -> bool {
    bytes.iter().fold(0u8, |acc, byte| acc.wrapping_add(*byte)) == 0
}

/// Validate an RSDP candidate; return the XSDT address it points to.
fn rsdp_xsdt_addr(bytes: &[u8]) -> Option<u64> {
    if bytes.len() < RSDP_LEN || &bytes[..8] != RSDP_SIG || !checksum_ok(&bytes[..20]) {
        return None;
    }
    // ACPI 2.0+: validate the extended checksum over the stated length.
    if bytes[15] >= 2 {
        let len = u32_at(bytes, 20) as usize;
        if len < RSDP_LEN || bytes.len() < len || !checksum_ok(&bytes[..len]) {
            return None;
        }
    }
    Some(u64_at(bytes, RSDP_XSDT_OFF))
}

/// XSDT walk that checks each candidate's signature through `read`.
fn find_fadt_in_xsdt(read: &impl Fn(u64, &mut [u8]), xsdt_addr: u64) -> Option<u64> {
    let mut header = [0u8; XSDT_ENTRY_OFF];
    read(xsdt_addr, &mut header);
    if &header[..4] != b"XSDT" {
        return None;
    }
    let len = (u32_at(&header, SDT_LEN_OFF) as usize).clamp(XSDT_ENTRY_OFF, MAX_XSDT_LEN);
    let mut bytes = [0u8; MAX_XSDT_LEN];
    read(xsdt_addr, &mut bytes[..len]);
    let entries = (len - XSDT_ENTRY_OFF) / 8;
    (0..entries)
        .map(|i| u64_at(&bytes, XSDT_ENTRY_OFF + i * 8))
        .filter(|&addr| addr != 0)
        .find(|&addr| {
            let mut sig = [0u8; 4];
            read(addr, &mut sig);
            &sig == b"FACP"
        })
}

/// FADT bytes → FACS address. Prefers the 64-bit pointer and falls back to
/// the 32-bit one for ACPI 1.0 tables.
fn fadt_facs_addr(bytes: &[u8]) -> Option<u64> {
    if bytes.len() < FADT_LEN || &bytes[..4] != b"FACP" {
        return None;
    }
    match u64_at(bytes, FADT_X_FIRMWARE_CTL_OFF) {
        0 => match u32_at(bytes, FADT_FIRMWARE_CTRL_OFF) {
            0 => None,
            addr => Some(u64::from(addr)),
        },
        addr => Some(addr),
    }
}

/// FACS bytes → firmware waking vector, if present and below 1 MiB.
fn facs_wake_vector(bytes: &[u8]) -> Option<u32> {
    if bytes.len() < FACS_LEN || &bytes[..4] != b"FACS" {
        return None;
    }
    match u32_at(bytes, FACS_WAKE_VECTOR_OFF) {
        0 => None,
        vector if vector < REAL_MODE_LIMIT => Some(vector),
        _ => None,
    }
}

/// Scan 16-byte-aligned candidates in `[lo, hi)` for a valid RSDP.
fn scan_rsdp(read: &impl Fn(u64, &mut [u8]), lo: u64, hi: u64) -> Option<u64> {
    let mut buf = [0u8; RSDP_LEN];
    (lo..hi).step_by(16).find_map(|addr| {
        read(addr, &mut buf);
        rsdp_xsdt_addr(&buf)
    })
}

/// Walk the surviving tables to the OS wake vector.
///
/// `read(addr, buf)` copies `buf.len()` bytes of physical memory at `addr`
/// into `buf`. S3-resident memory is untrusted: every span is signature and
/// checksum validated before use, and sizes are bounded.
///
/// Returns the 16-bit real-mode wake vector, or `None` when no valid chain
/// exists (caller then falls back to a clean cold boot).
pub fn find_wakeup_vector_with(read: impl Fn(u64, &mut [u8])) -> Option<u32> {
    // ACPI scan order: first KiB of the EBDA (from the BDA), then the legacy
    // BIOS window. The EBDA survives S3 (conventional memory, OS-reserved).
    let mut bda = [0u8; 2];
    read(BDA_EBDA_SEG_PTR, &mut bda);
    let ebda = u64::from(u16::from_le_bytes(bda)) << 4;
    let xsdt_addr = if (0x8_0000..0xA_0000).contains(&ebda) {
        scan_rsdp(&read, ebda, ebda + 0x400).or_else(|| scan_rsdp(&read, SCAN_LO, SCAN_HI))
    } else {
        scan_rsdp(&read, SCAN_LO, SCAN_HI)
    }?;

    let fadt_addr = find_fadt_in_xsdt(&read, xsdt_addr)?;

    let mut fadt = [0u8; FADT_LEN];
    read(fadt_addr, &mut fadt);
    let facs_addr = fadt_facs_addr(&fadt)?;

    let mut facs = [0u8; FACS_LEN];
    read(facs_addr, &mut facs);
    facs_wake_vector(&facs)
}

/// Firmware entry: read the surviving tables through the identity map.
///
/// # Safety
/// Requires the identity-mapped page tables the x86 stages run with, and
/// that low memory (BDA/EBDA) plus the ACPI table region are readable.
#[cfg(target_arch = "x86_64")]
pub fn find_wakeup_vector() -> Option<u32> {
    find_wakeup_vector_with(|addr, buf| {
        // SAFETY: the caller contract; RAM reads have no side effects.
        unsafe {
            core::ptr::copy_nonoverlapping(addr as usize as *const u8, buf.as_mut_ptr(), buf.len());
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::vec;
    use std::vec::Vec;

    /// Sparse fake physical memory served through the `read` closure.
    #[derive(Default)]
    struct FakeMem {
        pages: BTreeMap<u64, [u8; 4096]>,
    }

    impl FakeMem {
        fn write(&mut self, addr: u64, bytes: &[u8]) {
            for (i, byte) in bytes.iter().enumerate() {
                let a = addr + i as u64;
                self.pages.entry(a & !0xFFF).or_insert_with(|| [0u8; 4096])[(a & 0xFFF) as usize] =
                    *byte;
            }
        }

        fn reader(&self) -> impl Fn(u64, &mut [u8]) + '_ {
            |addr, buf| {
                for (i, byte) in buf.iter_mut().enumerate() {
                    let a = addr + i as u64;
                    *byte = self
                        .pages
                        .get(&(a & !0xFFF))
                        .map_or(0, |p| p[(a & 0xFFF) as usize]);
                }
            }
        }
    }

    fn fix_checksum(bytes: &mut [u8]) {
        let sum = bytes.iter().fold(0u8, |acc, byte| acc.wrapping_add(*byte));
        bytes[8] = bytes[8].wrapping_sub(sum);
    }

    fn rsdp(xsdt: u64) -> Vec<u8> {
        let mut b = vec![0u8; RSDP_LEN];
        b[..8].copy_from_slice(RSDP_SIG);
        b[15] = 2; // revision
        b[16..20].copy_from_slice(&0xF0000u32.to_le_bytes());
        b[20..24].copy_from_slice(&(RSDP_LEN as u32).to_le_bytes());
        b[24..32].copy_from_slice(&xsdt.to_le_bytes());
        fix_checksum(&mut b[..20]);
        // Extended checksum over the full length (stored at offset 32).
        let sum = b[..RSDP_LEN]
            .iter()
            .fold(0u8, |acc, byte| acc.wrapping_add(*byte));
        b[32] = 0u8.wrapping_sub(sum);
        b
    }

    fn xsdt(entries: &[u64]) -> Vec<u8> {
        let len = XSDT_ENTRY_OFF + entries.len() * 8;
        let mut b = vec![0u8; len];
        b[..4].copy_from_slice(b"XSDT");
        b[4..8].copy_from_slice(&(len as u32).to_le_bytes());
        for (i, entry) in entries.iter().enumerate() {
            b[XSDT_ENTRY_OFF + i * 8..XSDT_ENTRY_OFF + i * 8 + 8]
                .copy_from_slice(&entry.to_le_bytes());
        }
        b
    }

    fn fadt(facs: u64) -> Vec<u8> {
        let mut b = vec![0u8; FADT_LEN];
        b[..4].copy_from_slice(b"FACP");
        b[4..8].copy_from_slice(&(FADT_LEN as u32).to_le_bytes());
        b[FADT_X_FIRMWARE_CTL_OFF..FADT_X_FIRMWARE_CTL_OFF + 8]
            .copy_from_slice(&facs.to_le_bytes());
        b
    }

    /// ACPI 1.0 style table: only the 32-bit FACS pointer is present.
    fn fadt_legacy(facs: u32) -> Vec<u8> {
        let mut b = fadt(0);
        b[FADT_FIRMWARE_CTRL_OFF..FADT_FIRMWARE_CTRL_OFF + 4].copy_from_slice(&facs.to_le_bytes());
        b
    }

    fn facs(vector: u32) -> Vec<u8> {
        let mut b = vec![0u8; FACS_LEN];
        b[..4].copy_from_slice(b"FACS");
        b[4..8].copy_from_slice(&64u32.to_le_bytes());
        b[FACS_WAKE_VECTOR_OFF..FACS_WAKE_VECTOR_OFF + 4].copy_from_slice(&vector.to_le_bytes());
        b
    }

    /// EBDA at 0x9F000 (as fstart installs it), tables high in RAM.
    fn valid_mem() -> FakeMem {
        let mut mem = FakeMem::default();
        mem.write(0x40E, &0x9F00u16.to_le_bytes());
        mem.write(0x9F000, &rsdp(0x1_0000));
        mem.write(0x1_0000, &xsdt(&[0x2_0000]));
        mem.write(0x2_0000, &fadt(0x3_0000));
        mem.write(0x3_0000, &facs(0x8000));
        mem
    }

    #[test]
    fn finds_vector_through_ebda_chain() {
        let mem = valid_mem();
        assert_eq!(find_wakeup_vector_with(mem.reader()), Some(0x8000));
    }

    #[test]
    fn accepts_a_legacy_32bit_facs_pointer() {
        let mut mem = valid_mem();
        mem.write(0x2_0000, &fadt_legacy(0x3_0000));
        assert_eq!(find_wakeup_vector_with(mem.reader()), Some(0x8000));
    }

    /// The x_dsdt field must not be mistaken for the FACS pointer: the D41S
    /// FADT carries x_firmware_ctrl at 132 and x_dsdt at 140.
    #[test]
    fn ignores_dsdt_pointer() {
        let mut mem = valid_mem();
        mem.write(0x2_0000, &fadt(0x3_0000));
        mem.write(
            0x2_0000u64 + (FADT_LEN - 8) as u64,
            &0x3_0000u64.to_le_bytes(),
        );
        assert_eq!(find_wakeup_vector_with(mem.reader()), Some(0x8000));
    }

    #[test]
    fn falls_back_to_legacy_scan_window() {
        let mut mem = valid_mem();
        // No EBDA pointer; RSDP only in the 0xE0000..0xFFFFF window.
        mem.write(0x40E, &0u16.to_le_bytes());
        mem.write(0x9F000, &vec![0u8; 0x400]);
        mem.write(0xF_0000, &rsdp(0x1_0000));
        assert_eq!(find_wakeup_vector_with(mem.reader()), Some(0x8000));
    }

    #[test]
    fn rejects_bad_rsdp_checksum() {
        let mut mem = valid_mem();
        let mut bad = rsdp(0x1_0000);
        bad[9] ^= 0xFF; // corrupt OEMID without fixing checksums
        mem.write(0x9F000, &bad);
        mem.write(0x40E, &0u16.to_le_bytes());
        assert_eq!(find_wakeup_vector_with(mem.reader()), None);
    }

    #[test]
    fn rejects_zero_vector() {
        let mut mem = valid_mem();
        mem.write(0x3_0000, &facs(0));
        assert_eq!(find_wakeup_vector_with(mem.reader()), None);
    }

    #[test]
    fn rejects_vector_at_or_above_1mib() {
        let mut mem = valid_mem();
        mem.write(0x3_0000, &facs(0x10_0000));
        assert_eq!(find_wakeup_vector_with(mem.reader()), None);
    }

    #[test]
    fn rejects_missing_facs_signature() {
        let mut mem = valid_mem();
        mem.write(0x3_0000, &vec![0u8; FACS_LEN]);
        assert_eq!(find_wakeup_vector_with(mem.reader()), None);
    }

    #[test]
    fn rejects_garbage_after_g3() {
        let mem = FakeMem::default();
        assert_eq!(find_wakeup_vector_with(mem.reader()), None);
    }
}
