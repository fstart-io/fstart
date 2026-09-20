//! x86 discovery of the surviving ACPI RSDP used for S3 resume.
//!
//! The architecture-neutral ACPI table walk lives in [`crate::wake`]. This
//! module only implements the PC-specific BDA/EBDA and legacy BIOS-window RSDP
//! search plus the identity-mapped physical-memory reader.

/// BDA pointer to the EBDA segment (physical `0x40E`).
const BDA_EBDA_SEG_PTR: u64 = 0x40E;
/// Legacy RSDP scan window (top of conventional memory).
const SCAN_LO: u64 = 0xE_0000;
const SCAN_HI: u64 = 0xF_FFFF;
/// Wake vectors are 16-bit real-mode code, hence below 1 MiB.
const REAL_MODE_LIMIT: u32 = 0x10_0000;
#[cfg(test)]
const RSDP_LEN: usize = 36;

/// Scan 16-byte-aligned candidates in `[lo, hi)` for a valid RSDP.
fn scan_rsdp(read: &impl Fn(u64, &mut [u8]) -> bool, lo: u64, hi: u64) -> Option<u64> {
    (lo..hi)
        .step_by(16)
        .find(|addr| crate::wake::rsdp_is_valid_with(read, *addr))
}

/// Walk the surviving tables to the OS wake vector.
///
/// `read(addr, buf)` copies `buf.len()` bytes of physical memory at `addr`
/// into `buf`. S3-resident memory is untrusted: every span is signature and
/// checksum validated before use, and sizes are bounded.
///
/// Returns the 16-bit real-mode wake vector, or `None` when no valid chain
/// exists (caller then falls back to a clean cold boot).
pub fn find_wakeup_vector_with(read: impl Fn(u64, &mut [u8]) -> bool) -> Option<u32> {
    // ACPI scan order: first KiB of the EBDA (from the BDA), then the legacy
    // BIOS window. The EBDA survives S3 (conventional memory, OS-reserved).
    let mut bda = [0u8; 2];
    if !read(BDA_EBDA_SEG_PTR, &mut bda) {
        return None;
    }
    let ebda = u64::from(u16::from_le_bytes(bda)) << 4;
    let rsdp_addr = if (0x8_0000..0xA_0000).contains(&ebda) {
        scan_rsdp(&read, ebda, ebda + 0x400).or_else(|| scan_rsdp(&read, SCAN_LO, SCAN_HI))
    } else {
        scan_rsdp(&read, SCAN_LO, SCAN_HI)
    }?;
    crate::wake::wakeup_vector_from_rsdp_with(&read, rsdp_addr)
        .filter(|vector| *vector < REAL_MODE_LIMIT)
}

/// Firmware entry: read surviving tables through the low-4-GiB identity map.
///
/// `readable` must approve only physical-memory spans that firmware may read;
/// rejected or overflowing table pointers make the walk fail closed.
#[cfg(target_arch = "x86_64")]
pub fn find_wakeup_vector(readable: impl Fn(u64, usize) -> bool) -> Option<u32> {
    find_wakeup_vector_with(|addr, buf| {
        let Some(end) = addr.checked_add(buf.len() as u64) else {
            return false;
        };
        if end > 0x1_0000_0000 || !readable(addr, buf.len()) {
            return false;
        }
        // SAFETY: the checked span lies inside the installed low-4-GiB
        // identity map; `copy` also permits a corrupt source to overlap this
        // stack buffer, and RAM reads have no side effects.
        unsafe {
            core::ptr::copy(addr as usize as *const u8, buf.as_mut_ptr(), buf.len());
        }
        true
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::vec;
    use std::vec::Vec;

    const RSDP_SIG: &[u8; 8] = b"RSD PTR ";
    const XSDT_ENTRY_OFF: usize = 36;
    const FADT_LEN: usize = 148;
    const FADT_FIRMWARE_CTRL_OFF: usize = 36;
    const FADT_X_FIRMWARE_CTL_OFF: usize = 132;
    const FACS_LEN: usize = 16;
    const FACS_WAKE_VECTOR_OFF: usize = 12;

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

        fn reader(&self) -> impl Fn(u64, &mut [u8]) -> bool + '_ {
            |addr, buf| {
                for (i, byte) in buf.iter_mut().enumerate() {
                    let a = addr + i as u64;
                    *byte = self
                        .pages
                        .get(&(a & !0xFFF))
                        .map_or(0, |p| p[(a & 0xFFF) as usize]);
                }
                true
            }
        }
    }

    fn fix_checksum(bytes: &mut [u8]) {
        let sum = bytes.iter().fold(0u8, |acc, byte| acc.wrapping_add(*byte));
        bytes[8] = bytes[8].wrapping_sub(sum);
    }

    fn fix_sdt_checksum(bytes: &mut [u8]) {
        bytes[9] = 0;
        let sum = bytes.iter().fold(0u8, |acc, byte| acc.wrapping_add(*byte));
        bytes[9] = 0u8.wrapping_sub(sum);
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
        fix_sdt_checksum(&mut b);
        b
    }

    fn fadt(facs: u64) -> Vec<u8> {
        let mut b = vec![0u8; FADT_LEN];
        b[..4].copy_from_slice(b"FACP");
        b[4..8].copy_from_slice(&(FADT_LEN as u32).to_le_bytes());
        b[FADT_X_FIRMWARE_CTL_OFF..FADT_X_FIRMWARE_CTL_OFF + 8]
            .copy_from_slice(&facs.to_le_bytes());
        fix_sdt_checksum(&mut b);
        b
    }

    /// ACPI 1.0 style table: only the 32-bit FACS pointer is present.
    fn fadt_legacy(facs: u32) -> Vec<u8> {
        let mut b = fadt(0);
        b[FADT_FIRMWARE_CTRL_OFF..FADT_FIRMWARE_CTRL_OFF + 4].copy_from_slice(&facs.to_le_bytes());
        fix_sdt_checksum(&mut b);
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
        let mut table = fadt(0x3_0000);
        table[FADT_LEN - 8..].copy_from_slice(&0x3_0000u64.to_le_bytes());
        fix_sdt_checksum(&mut table);
        mem.write(0x2_0000, &table);
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
    fn rejects_unreadable_table_spans() {
        let mem = valid_mem();
        let reader = mem.reader();
        assert_eq!(
            find_wakeup_vector_with(|addr, buf| addr != 0x1_0000 && reader(addr, buf)),
            None
        );
    }

    #[test]
    fn rejects_corrupt_sdt_checksums() {
        let mut mem = valid_mem();
        let mut table = xsdt(&[0x2_0000]);
        table[10] ^= 0xff;
        mem.write(0x1_0000, &table);
        assert_eq!(find_wakeup_vector_with(mem.reader()), None);

        let mut mem = valid_mem();
        let mut table = fadt(0x3_0000);
        table[10] ^= 0xff;
        mem.write(0x2_0000, &table);
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
