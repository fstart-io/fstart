//! Error records as ACPI hands them to the OS: generic error status blocks
//! (ACPI 6.5 §18.3.2.7.1, the BERT's boot error region and a GHES's
//! status) of generic error data entries, whose sections are the UEFI
//! common platform error record's (UEFI 2.10 appendix N).
//! `acpi_tables::hest` has these types, but its data entry takes a 16-bit
//! section type and cannot carry a section.

use alloc::vec::Vec;
use zerocopy::little_endian::{U16, U32, U64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

#[repr(C)]
#[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
struct GenericErrorStatusHeader {
    block_status: U32,
    raw_data_offset: U32,
    raw_data_length: U32,
    data_length: U32,
    error_severity: U32,
}

#[repr(C)]
#[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
struct GenericErrorDataEntryHeader {
    section_type: [u8; 16],
    error_severity: U32,
    revision: U16,
    validation_bits: u8,
    flags: u8,
    error_data_length: U32,
    fru_id: [u8; 16],
    fru_text: [u8; 20],
    timestamp: [u8; 8],
}

#[repr(C)]
#[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
struct ProcessorIaPrefix {
    validation: U64,
    lapic_id: U64,
    cpuid: [U64; 6],
}

#[repr(C)]
#[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
struct ProcessorIaErrorInfo {
    err_type: [u8; 16],
    reserved: [u8; 48],
}

#[repr(C)]
#[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
struct ProcessorIaContextHeader {
    context_type: U16,
    array_size: U16,
    msr_address: U32,
    mm_register_address: U64,
}

#[repr(C)]
#[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
struct PlatformMemorySection {
    validation: U64,
    error_status: U64,
    physical_address: U64,
    physical_address_mask: U64,
    node: U16,
    card: U16,
    module: U16,
    bank: U16,
    device: U16,
    row: U16,
    column: U16,
    bit_position: U16,
    requestor_id: U64,
    responder_id: U64,
    target_id: U64,
    memory_error_type: u8,
    extended: u8,
    rank_number: U16,
    card_handle: U16,
    module_handle: U16,
}

#[repr(C)]
#[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
struct ProcessorGenericSection {
    validation: U64,
    processor_type: u8,
    processor_isa: u8,
    error_type: u8,
    operation: u8,
    flags: u8,
    level: u8,
    reserved: U16,
    cpu_version: U64,
    cpu_brand: [u8; 48],
    tail: [u8; 120],
}

const _: () = assert!(core::mem::size_of::<GenericErrorStatusHeader>() == STATUS_HEADER);
const _: () = assert!(core::mem::size_of::<GenericErrorDataEntryHeader>() == ENTRY_HEADER);
const _: () = assert!(core::mem::size_of::<ProcessorIaPrefix>() == 64);
const _: () = assert!(core::mem::size_of::<ProcessorIaErrorInfo>() == 64);
const _: () = assert!(core::mem::size_of::<ProcessorIaContextHeader>() == 16);
const _: () = assert!(core::mem::size_of::<PlatformMemorySection>() == 80);
const _: () = assert!(core::mem::size_of::<ProcessorGenericSection>() == 192);

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

/// The platform memory error section (UEFI N.2.5).
pub const PLATFORM_MEMORY: [u8; 16] = guid(
    0xa5bc_1114,
    0x6f64,
    0x4ede,
    [0xb8, 0x63, 0x3e, 0x83, 0xed, 0x7c, 0x83, 0xb1],
);
/// The processor generic error section (UEFI N.2.4.1).
pub const PROCESSOR_GENERIC: [u8; 16] = guid(
    0x9876_ccad,
    0x47b4,
    0x4bdb,
    [0xb6, 0x5e, 0x16, 0xf1, 0x93, 0xc4, 0xf3, 0xdb],
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
    /// The FRU's text (padded with NULs), if any.
    pub fru_text: Option<[u8; 20]>,
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
    out.extend_from_slice(
        GenericErrorStatusHeader {
            block_status: U32::new(block_status),
            raw_data_offset: U32::ZERO,
            raw_data_length: U32::ZERO,
            data_length: U32::new(data_length as u32),
            error_severity: U32::new(severity as u32),
        }
        .as_bytes(),
    );
    for entry in entries {
        // Revision 3.0; validation bit 1: the FRU text. No FRU ID or
        // timestamp.
        out.extend_from_slice(
            GenericErrorDataEntryHeader {
                section_type: entry.section,
                error_severity: U32::new(entry.severity as u32),
                revision: U16::new(0x300),
                validation_bits: u8::from(entry.fru_text.is_some()) << 1,
                flags: 0,
                error_data_length: U32::new(entry.data.len() as u32),
                fru_id: [0; 16],
                fru_text: entry.fru_text.unwrap_or([0; 20]),
                timestamp: [0; 8],
            }
            .as_bytes(),
        );
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
    out.extend_from_slice(
        ProcessorIaPrefix {
            validation: U64::new(validation),
            lapic_id: U64::new(apic_id),
            cpuid: [
                U64::new(u64::from(cpuid[0])),
                U64::new(u64::from(cpuid[1])),
                U64::new(u64::from(cpuid[2])),
                U64::new(u64::from(cpuid[3])),
                U64::ZERO,
                U64::ZERO,
            ],
        }
        .as_bytes(),
    );
    // The error information: its type only.
    out.extend_from_slice(
        ProcessorIaErrorInfo {
            err_type: MS_CHECK,
            reserved: [0; 48],
        }
        .as_bytes(),
    );
    // The context: MSR registers.
    let size = 8 * registers.len();
    out.extend_from_slice(
        ProcessorIaContextHeader {
            context_type: U16::new(1),
            array_size: U16::new(size as u16),
            msr_address: U32::new(msr),
            mm_register_address: U64::ZERO,
        }
        .as_bytes(),
    );
    for &register in registers {
        out.extend_from_slice(U64::new(register).as_bytes());
    }
    // Context structures are 16-byte aligned.
    out.resize(out.len().next_multiple_of(16), 0);
    out
}

/// What a [`platform_memory`] section names; `None` is not valid.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemoryError {
    /// The error status (UEFI N.2.11): its type in bits 15:8, the
    /// address/control/data/responder/requester flags in bits 20:16.
    pub status: Option<u64>,
    pub node: Option<u16>,
    pub card: Option<u16>,
    pub module: Option<u16>,
    /// The memory error type (UEFI table N.31), e.g. 15: a physical
    /// memory map-out event.
    pub kind: Option<u8>,
}

/// The error status of a memory error (UEFI N.2.11): ERR_MEM, in the
/// data.
pub const MEMORY_DATA_ERROR: u64 = 0x04 << 8 | 1 << 18;
/// The memory error type of a DIMM taken out of the memory map.
pub const MEMORY_MAP_OUT: u8 = 15;

/// A platform memory error section (UEFI N.2.5, 80 bytes).
pub fn platform_memory(error: &MemoryError) -> Vec<u8> {
    let mut validation = 0u64;
    if error.status.is_some() {
        validation |= 1 << 0;
    }
    if error.node.is_some() {
        validation |= 1 << 3;
    }
    if error.card.is_some() {
        validation |= 1 << 4;
    }
    if error.module.is_some() {
        validation |= 1 << 5;
    }
    if error.kind.is_some() {
        validation |= 1 << 14;
    }
    PlatformMemorySection {
        validation: U64::new(validation),
        error_status: U64::new(error.status.unwrap_or(0)),
        physical_address: U64::ZERO,
        physical_address_mask: U64::ZERO,
        node: U16::new(error.node.unwrap_or(0)),
        card: U16::new(error.card.unwrap_or(0)),
        module: U16::new(error.module.unwrap_or(0)),
        bank: U16::ZERO,
        device: U16::ZERO,
        row: U16::ZERO,
        column: U16::ZERO,
        bit_position: U16::ZERO,
        requestor_id: U64::ZERO,
        responder_id: U64::ZERO,
        target_id: U64::ZERO,
        memory_error_type: error.kind.unwrap_or(0),
        extended: 0,
        rank_number: U16::ZERO,
        card_handle: U16::ZERO,
        module_handle: U16::ZERO,
    }
    .as_bytes()
    .to_vec()
}

/// A processor generic error section (UEFI N.2.4.1, 192 bytes) of an
/// x64 processor (CPUID leaf 1 EAX `version`, brand string `brand`, from
/// leaves 8000_0002h-4) of an unknown error type and operation. No
/// target, requestor or responder address: a caller with data it cannot
/// name as one keeps it in a section of its own.
pub fn processor_generic(version: u32, brand: &[u8; 48]) -> Vec<u8> {
    ProcessorGenericSection {
        // Processor type, ISA, error type, operation, CPU version, brand (the
        // vendors' 2CFh without its target address).
        validation: U64::new(0xcf),
        // IA32/x64, x64.
        processor_type: 0,
        processor_isa: 2,
        error_type: 0,
        operation: 0,
        flags: 0,
        level: 0,
        reserved: U16::ZERO,
        cpu_version: U64::new(u64::from(version)),
        cpu_brand: *brand,
        tail: [0; 120],
    }
    .as_bytes()
    .to_vec()
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
            fru_text: None,
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
            fru_text: None,
        };
        let block = status_block(&[
            entry(Severity::Corrected),
            entry(Severity::Recoverable),
            entry(Severity::Corrected),
        ]);
        assert_eq!(block[0], 0b1011 | 3 << 4);
        assert_eq!(&block[16..20], &0u32.to_le_bytes());
    }

    #[test]
    fn a_dimm_mapped_out_as_the_vendors_record_it() {
        let section = platform_memory(&MemoryError {
            status: Some(MEMORY_DATA_ERROR),
            node: Some(1),
            card: Some(3),
            module: Some(1),
            kind: Some(MEMORY_MAP_OUT),
        });
        // The vendors' validation 4039h and status 40400h.
        assert_eq!(&section[..8], &0x4039u64.to_le_bytes());
        assert_eq!(&section[8..16], &0x40400u64.to_le_bytes());
        assert_eq!(&section[32..38], &[1, 0, 3, 0, 1, 0]);
        assert_eq!(section[72], 15);
        let mut fru = [0; 20];
        fru[..4].copy_from_slice(b"DIMM");
        let block = status_block(&[Entry {
            section: PLATFORM_MEMORY,
            severity: Severity::Fatal,
            data: section,
            fru_text: Some(fru),
        }]);
        assert_eq!(block.len(), 20 + 72 + 80);
        assert_eq!(&block[20..36], &PLATFORM_MEMORY);
        assert_eq!(block[42], 2, "FRU text valid");
        assert_eq!(&block[20 + 44..20 + 48], b"DIMM");
        assert_eq!(&PLATFORM_MEMORY[..4], &[0x14, 0x11, 0xbc, 0xa5]);
        let generic = processor_generic(0xa10f11, &[b'A'; 48]);
        // No target address valid (bit 9), none in it.
        assert_eq!((generic.len(), &generic[..2]), (192, &[0xcf, 0][..]));
        assert_eq!(&generic[160..168], &[0; 8]);
    }
}
