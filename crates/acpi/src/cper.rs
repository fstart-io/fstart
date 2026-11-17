//! Error records as ACPI hands them to the OS: generic error status blocks
//! (ACPI 6.5 §18.3.2.7.1, the BERT's boot error region and a GHES's
//! status) of generic error data entries, whose sections are the UEFI
//! common platform error record's (UEFI 2.10 appendix N).
//! `acpi_tables::hest` has these types, but its data entry takes a 16-bit
//! section type and cannot carry a section.

use alloc::vec::Vec;

/// Error severity (ACPI 6.5 table 18.11, UEFI N.2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Severity {
    Recoverable = 0,
    Fatal = 1,
    Corrected = 2,
    Informational = 3,
}

/// The IA32/x64 processor error section (UEFI N.2.4.2).
pub const PROCESSOR_IA: [u8; 16] = guid(
    0xdc3e_a0b0,
    0xa144,
    0x4797,
    [0xb9, 0x5b, 0x53, 0xfa, 0x24, 0x2b, 0x6e, 0x1d],
);
/// The microarchitecture-specific check of an IA32/x64 error information
/// structure (UEFI N.2.4.2.1).
pub const MS_CHECK: [u8; 16] = guid(
    0x48ab_7f57,
    0xdc34,
    0x4f6c,
    [0xa7, 0xd3, 0xb0, 0xb5, 0xb0, 0xa7, 0x43, 0x14],
);

/// A GUID in its little-endian byte layout.
pub const fn guid(a: u32, b: u16, c: u16, d: [u8; 8]) -> [u8; 16] {
    let a = a.to_le_bytes();
    let b = b.to_le_bytes();
    let c = c.to_le_bytes();
    [
        a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d[0], d[1], d[2], d[3], d[4], d[5], d[6],
        d[7],
    ]
}

/// A generic error data entry (ACPI 6.5 table 18.13, revision 0x300): a
/// section and its severity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub section: [u8; 16],
    pub severity: Severity,
    pub data: Vec<u8>,
}

/// A generic error data entry's header (revision 0x300).
pub const ENTRY_HEADER: usize = 72;
/// A generic error status block's header.
pub const STATUS_HEADER: usize = 20;

impl Entry {
    /// Its length in a status block, header included.
    pub fn encoded_len(&self) -> usize {
        ENTRY_HEADER + self.data.len()
    }
}

/// A generic error status block of `entries`, its block status set from
/// their severities and its severity the worst of them.
pub fn status_block(entries: &[Entry]) -> Vec<u8> {
    let uncorrected = entries
        .iter()
        .filter(|e| e.severity <= Severity::Fatal)
        .count();
    let corrected = entries
        .iter()
        .filter(|e| e.severity == Severity::Corrected)
        .count();
    let block_status = u32::from(uncorrected > 0)
        | u32::from(corrected > 0) << 1
        | u32::from(uncorrected > 1) << 2
        | u32::from(corrected > 1) << 3
        | (entries.len() as u32 & 0x3ff) << 4;
    let severity = entries
        .iter()
        .map(|e| e.severity)
        .min_by_key(|s| match s {
            Severity::Fatal => 0,
            Severity::Recoverable => 1,
            Severity::Corrected => 2,
            Severity::Informational => 3,
        })
        .unwrap_or(Severity::Informational);
    let data_length: usize = entries.iter().map(Entry::encoded_len).sum();
    let mut out = Vec::with_capacity(STATUS_HEADER + data_length);
    for value in [block_status, 0, 0, data_length as u32, severity as u32] {
        out.extend_from_slice(&value.to_le_bytes());
    }
    for entry in entries {
        out.extend_from_slice(&entry.section);
        out.extend_from_slice(&(entry.severity as u32).to_le_bytes());
        // Revision 3.0; no FRU ID, FRU text or timestamp.
        out.extend_from_slice(&0x300u16.to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(&(entry.data.len() as u32).to_le_bytes());
        out.extend_from_slice(&[0; 16 + 20 + 8]);
        out.extend_from_slice(&entry.data);
    }
    out
}

/// An IA32/x64 processor error section (UEFI N.2.4.2) of the CPU with
/// local APIC ID `apic_id` and CPUID leaf 1 `cpuid` (EAX, EBX, ECX, EDX):
/// one microarchitecture-specific error information structure and one
/// MSR context, `registers` read from `msr` on.
///
/// Linux hands such a section with an MCA bank's registers from its
/// MCA_STATUS on (the SMCA layout: STATUS, ADDR, MISC0, CONFIG, IPID,
/// SYND, reserved, DESTAT, DEADDR, MISC1..4, SYND1, SYND2) to its
/// machine-check decoding (`apei_smca_report_x86_error`).
pub fn processor_ia(apic_id: u64, cpuid: [u32; 4], msr: u32, registers: &[u64]) -> Vec<u8> {
    let mut out = Vec::new();
    // LAPIC ID and CPUID valid (bits 0, 1); one error information (bits
    // 7:2), one context (bits 13:8).
    let validation: u64 = 1 | 1 << 1 | 1 << 2 | 1 << 8;
    out.extend_from_slice(&validation.to_le_bytes());
    out.extend_from_slice(&apic_id.to_le_bytes());
    let mut id = [0u8; 48];
    for (slot, value) in id.chunks_mut(8).zip(cpuid) {
        slot[..4].copy_from_slice(&value.to_le_bytes());
    }
    out.extend_from_slice(&id);
    // The error information: its type only.
    out.extend_from_slice(&MS_CHECK);
    out.extend_from_slice(&[0; 48]);
    // The context: MSR registers.
    let size = 8 * registers.len();
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&(size as u16).to_le_bytes());
    out.extend_from_slice(&msr.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());
    for register in registers {
        out.extend_from_slice(&register.to_le_bytes());
    }
    // Context structures are 16-byte aligned.
    out.resize(out.len().next_multiple_of(16), 0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_processor_section_in_a_status_block() {
        let section = processor_ia(
            0x21,
            [0xa10f11, 0, 0, 0],
            0xc000_2151,
            &[0xbc00_0000_0000_0135; 16],
        );
        assert_eq!(section.len(), 64 + 64 + 16 + 128);
        assert_eq!(&section[..8], &(0x107u64).to_le_bytes());
        assert_eq!(&section[8..16], &0x21u64.to_le_bytes());
        // The context header: MSR registers, 128 bytes, from C000_2151h.
        assert_eq!(&section[128..136], &[1, 0, 128, 0, 0x51, 0x21, 0, 0xc0]);
        let block = status_block(&[Entry {
            section: PROCESSOR_IA,
            severity: Severity::Fatal,
            data: section.clone(),
        }]);
        // Uncorrected, one entry; data length; fatal.
        assert_eq!(&block[..4], &(1u32 | 1 << 4).to_le_bytes());
        assert_eq!(&block[12..16], &((72 + section.len()) as u32).to_le_bytes());
        assert_eq!(&block[16..20], &1u32.to_le_bytes());
        assert_eq!(&block[20..36], &PROCESSOR_IA);
        assert_eq!(&block[40..42], &[0, 3]);
        assert_eq!(&block[44..48], &(section.len() as u32).to_le_bytes());
        assert_eq!(&block[92..], &section[..]);
        assert_eq!(&PROCESSOR_IA[..4], &[0xb0, 0xa0, 0x3e, 0xdc]);
    }

    #[test]
    fn severity_and_status_of_mixed_entries() {
        let entry = |severity| Entry {
            section: PROCESSOR_IA,
            severity,
            data: Vec::new(),
        };
        let block = status_block(&[
            entry(Severity::Corrected),
            entry(Severity::Recoverable),
            entry(Severity::Corrected),
        ]);
        assert_eq!(block[0], 0b1011 | 3 << 4);
        assert_eq!(&block[16..20], &0u32.to_le_bytes());
    }
}
