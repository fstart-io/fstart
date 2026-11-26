//! Error records as ACPI hands them to the OS: generic error status blocks
//! (ACPI 6.5 §18.3.2.7.1, the BERT's boot error region and a GHES's
//! status) of generic error data entries, whose sections are the UEFI
//! common platform error record's (UEFI 2.10 appendix N).
//! `acpi_tables::hest` has these types, but its data entry takes a 16-bit
//! section type and cannot carry a section.
//!
//! A firmware-first handler (SMM, no heap, no panic) writes its records
//! into a GHES's status block in place: [`append`] with the sections of
//! [`pcie`] and [`cxl_protocol`], which encode into fixed buffers.

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

/// The PCI Express error section (UEFI N.2.7).
pub const PCIE: [u8; 16] = guid(
    0xd995_e954,
    0xbbc1,
    0x430f,
    [0xad, 0x91, 0xb4, 0x4d, 0xcb, 0x3c, 0x6f, 0x35],
);
/// The CXL protocol error section (UEFI N.2.13).
pub const CXL_PROTOCOL: [u8; 16] = guid(
    0x80b9_efb4,
    0x52b5,
    0x4de3,
    [0xa7, 0x77, 0x68, 0x78, 0x4b, 0x77, 0x10, 0x48],
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

/// Severities in the order of their weight: the worst first.
pub const fn rank(severity: Severity) -> u8 {
    match severity {
        Severity::Fatal => 0,
        Severity::Recoverable => 1,
        Severity::Corrected => 2,
        Severity::Informational => 3,
    }
}

/// The header of a generic error data entry (revision 0x300) of
/// `section`, `severity`, `length` bytes of section data and the FRU text
/// (validation bit 1) if any; no FRU ID, no timestamp.
pub fn entry_header(
    section: &[u8; 16],
    severity: Severity,
    length: u32,
    fru_text: Option<&[u8; 20]>,
) -> [u8; ENTRY_HEADER] {
    let mut out = [0u8; ENTRY_HEADER];
    put(&mut out, 0, section);
    put(&mut out, 16, &(severity as u32).to_le_bytes());
    put(&mut out, 20, &0x300u16.to_le_bytes());
    put(&mut out, 22, &[u8::from(fru_text.is_some()) << 1, 0]);
    put(&mut out, 24, &length.to_le_bytes());
    if let Some(text) = fru_text {
        put(&mut out, 44, text);
    }
    out
}

/// Copy `bytes` into `out` at `at`, nothing if it does not fit (no
/// panic: SMM handlers encode with this).
#[inline]
fn put(out: &mut [u8], at: usize, bytes: &[u8]) {
    if let Some(to) = at
        .checked_add(bytes.len())
        .and_then(|end| out.get_mut(at..end))
    {
        to.iter_mut().zip(bytes).for_each(|(to, from)| *to = *from);
    }
}

/// A little-endian dword of `bytes` at `at`, 0 past the end.
#[inline]
fn dword(bytes: &[u8], at: usize) -> u32 {
    let mut value = [0u8; 4];
    if let Some(from) = bytes.get(at..at.saturating_add(4)) {
        value
            .iter_mut()
            .zip(from)
            .for_each(|(to, from)| *to = *from);
    }
    u32::from_le_bytes(value)
}

/// A status block has no room for the entry ([`append`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Full;

/// Append a data entry of `section` (`severity`, `data`, the FRU text if
/// any) to the generic error status block in `block`, in place, as a GHES's
/// producer does: a block whose status the OS has cleared (0) starts empty;
/// the entry goes after the data already there; then the data length, the
/// block's severity (the worst) and, last, the block status (uncorrectable
/// or correctable valid, "multiple" for a second of its kind, the entry
/// count). An OS reading meanwhile sees the old block or the new one. No
/// room: nothing changes.
pub fn append(
    block: &mut [u8],
    section: &[u8; 16],
    severity: Severity,
    data: &[u8],
    fru_text: Option<&[u8; 20]>,
) -> Result<(), Full> {
    use core::sync::atomic::{Ordering, fence};
    let status = dword(block, 0);
    let (length, worst) = if status == 0 {
        (0, Severity::Informational)
    } else {
        let worst = match dword(block, 16) {
            0 => Severity::Recoverable,
            1 => Severity::Fatal,
            2 => Severity::Corrected,
            _ => Severity::Informational,
        };
        (dword(block, 12) as usize, worst)
    };
    let at = STATUS_HEADER.checked_add(length).ok_or(Full)?;
    let end = at
        .checked_add(ENTRY_HEADER)
        .and_then(|end| end.checked_add(data.len()))
        .ok_or(Full)?;
    if end > block.len() {
        return Err(Full);
    }
    let header = entry_header(section, severity, data.len() as u32, fru_text);
    put(block, at, &header);
    put(block, at + ENTRY_HEADER, data);
    if status == 0 {
        // Raw data offset and length.
        put(block, 4, &[0; 8]);
    }
    fence(Ordering::Release);
    put(block, 12, &((end - STATUS_HEADER) as u32).to_le_bytes());
    let severity = if rank(severity) < rank(worst) {
        severity
    } else {
        worst
    };
    put(block, 16, &(severity as u32).to_le_bytes());
    let entries = (status >> 4 & 0x3ff).saturating_add(1).min(0x3ff);
    let (valid, multiple) = match header[16] {
        0 | 1 => (1, 1 << 2),
        _ => (1 << 1, 1 << 3),
    };
    let flags = status & 0xf | if status & valid != 0 { multiple } else { valid };
    fence(Ordering::Release);
    put(block, 0, &(flags | entries << 4).to_le_bytes());
    Ok(())
}

/// An empty status block's 20 bytes: what a GHES's block is set to at boot.
pub const EMPTY_STATUS: [u8; STATUS_HEADER] = [0; STATUS_HEADER];

/// What a PCI Express error section names its device by (UEFI N.2.7,
/// "Device ID").
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PcieDevice {
    pub vendor: u16,
    pub device: u16,
    /// Programming interface, subclass, class.
    pub class: [u8; 3],
    pub function: u8,
    pub device_number: u8,
    pub segment: u16,
    /// The bus of an endpoint, a bridge's primary bus.
    pub primary_bus: u8,
    /// A bridge's secondary bus.
    pub secondary_bus: u8,
    /// The slot number in bits 15:3 (Slot Capabilities' physical slot
    /// number, as the vendors record it).
    pub slot: u16,
}

/// A PCI Express error section's content; `None` (or empty) fields are
/// not valid.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PcieError<'a> {
    /// The PCI Express capability's device/port type.
    pub port_type: Option<u32>,
    pub version: Option<u32>,
    /// Command (low) and Status (high).
    pub command_status: Option<u32>,
    pub device: Option<PcieDevice>,
    pub serial: Option<u64>,
    /// Secondary Status (low) and Bridge Control (high).
    pub bridge: Option<u32>,
    /// The PCI Express capability's dwords (up to 60 bytes).
    pub capability: &'a [u32],
    /// The AER capability's dwords (up to 96 bytes).
    pub aer: &'a [u32],
}

/// Bytes of a PCI Express error section.
pub const PCIE_SECTION: usize = 208;

/// A PCI Express error section (UEFI N.2.7): what Linux hands to its AER
/// recovery (`ghes_handle_aer`) when the device ID and the AER info are
/// valid.
pub fn pcie(error: &PcieError<'_>) -> [u8; PCIE_SECTION] {
    let mut out = [0u8; PCIE_SECTION];
    let mut valid = 0u64;
    if let Some(port_type) = error.port_type {
        valid |= 1;
        put(&mut out, 8, &port_type.to_le_bytes());
    }
    if let Some(version) = error.version {
        valid |= 1 << 1;
        put(&mut out, 12, &version.to_le_bytes());
    }
    if let Some(command_status) = error.command_status {
        valid |= 1 << 2;
        put(&mut out, 16, &command_status.to_le_bytes());
    }
    if let Some(d) = error.device {
        valid |= 1 << 3;
        put(&mut out, 24, &d.vendor.to_le_bytes());
        put(&mut out, 26, &d.device.to_le_bytes());
        put(&mut out, 28, &d.class);
        put(&mut out, 31, &[d.function, d.device_number]);
        put(&mut out, 33, &d.segment.to_le_bytes());
        put(&mut out, 35, &[d.primary_bus, d.secondary_bus]);
        put(&mut out, 37, &d.slot.to_le_bytes());
    }
    if let Some(serial) = error.serial {
        valid |= 1 << 4;
        put(&mut out, 40, &serial.to_le_bytes());
    }
    if let Some(bridge) = error.bridge {
        valid |= 1 << 5;
        put(&mut out, 48, &bridge.to_le_bytes());
    }
    if !error.capability.is_empty() {
        valid |= 1 << 6;
        put_dwords(&mut out, 52, 60, error.capability);
    }
    if !error.aer.is_empty() {
        valid |= 1 << 7;
        put_dwords(&mut out, 112, 96, error.aer);
    }
    put(&mut out, 0, &valid.to_le_bytes());
    out
}

/// `dwords` at `at`, at most `room` bytes of them.
fn put_dwords(out: &mut [u8], at: usize, room: usize, dwords: &[u32]) {
    for (n, value) in dwords.iter().take(room / 4).enumerate() {
        put(out, at + 4 * n, &value.to_le_bytes());
    }
}

/// A CXL agent (UEFI N.2.13 "CXL Agent Type") and its address: an RCH
/// downstream port by its RCRB, any other by segment, bus, device and
/// function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CxlAgent {
    /// CXL 1.1 device (RCD).
    Rcd { segment: u16, bus: u8, devfn: u8 },
    /// CXL 1.1 host downstream port (RCH DP).
    RchDownstreamPort { rcrb: u64 },
    /// CXL 2.0+ device.
    Device { segment: u16, bus: u8, devfn: u8 },
    /// CXL 2.0+ root port.
    RootPort { segment: u16, bus: u8, devfn: u8 },
    /// CXL 2.0+ switch downstream port.
    DownstreamPort { segment: u16, bus: u8, devfn: u8 },
    /// CXL 2.0+ switch upstream port.
    UpstreamPort { segment: u16, bus: u8, devfn: u8 },
}

impl CxlAgent {
    fn encode(&self) -> (u8, u64) {
        let sbdf = |kind: u8, segment: u16, bus: u8, devfn: u8| {
            (
                kind,
                u64::from(devfn & 7)
                    | u64::from(devfn >> 3) << 8
                    | u64::from(bus) << 16
                    | u64::from(segment) << 24,
            )
        };
        match *self {
            Self::Rcd {
                segment,
                bus,
                devfn,
            } => sbdf(0, segment, bus, devfn),
            Self::RchDownstreamPort { rcrb } => (1, rcrb),
            Self::Device {
                segment,
                bus,
                devfn,
            } => sbdf(2, segment, bus, devfn),
            Self::RootPort {
                segment,
                bus,
                devfn,
            } => sbdf(5, segment, bus, devfn),
            Self::DownstreamPort {
                segment,
                bus,
                devfn,
            } => sbdf(6, segment, bus, devfn),
            Self::UpstreamPort {
                segment,
                bus,
                devfn,
            } => sbdf(7, segment, bus, devfn),
        }
    }
}

/// What a CXL protocol error section names its device by.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CxlDevice {
    pub vendor: u16,
    pub device: u16,
    pub subsystem_vendor: u16,
    pub subsystem: u16,
    /// Subclass, class.
    pub class: [u8; 2],
    /// The slot number in bits 15:3.
    pub slot: u16,
}

/// A CXL protocol error section's content; `None` (or empty) fields are
/// not valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CxlProtocolError<'a> {
    pub agent: CxlAgent,
    pub device: Option<CxlDevice>,
    pub serial: Option<u64>,
    /// The PCI Express capability's dwords (up to 60 bytes).
    pub capability: &'a [u32],
    /// The agent's CXL DVSEC (the device's or the Flex Bus port's) dwords.
    pub dvsec: &'a [u32],
    /// The CXL RAS capability's dwords (0x58 bytes).
    pub error_log: &'a [u32],
}

/// Bytes of a CXL protocol error section before its DVSEC and error log.
pub const CXL_PROTOCOL_HEADER: usize = 116;

/// A CXL protocol error section (UEFI N.2.13, Linux `struct
/// cxl_cper_sec_prot_err`) in `out`: the fixed part, then the DVSEC and the
/// RAS capability (error log). Returns its length; `None` if `out` is too
/// short.
pub fn cxl_protocol(out: &mut [u8], error: &CxlProtocolError<'_>) -> Option<usize> {
    let dvsec = 4 * error.dvsec.len();
    let log = 4 * error.error_log.len();
    let length = CXL_PROTOCOL_HEADER + dvsec + log;
    let out = out.get_mut(..length)?;
    out.iter_mut().for_each(|b| *b = 0);
    // Agent type and address.
    let mut valid = 0b11u64;
    let (kind, address) = error.agent.encode();
    put(out, 8, &[kind]);
    put(out, 16, &address.to_le_bytes());
    if let Some(d) = error.device {
        valid |= 1 << 2;
        for (n, half) in [d.vendor, d.device, d.subsystem_vendor, d.subsystem]
            .iter()
            .enumerate()
        {
            put(out, 24 + 2 * n, &half.to_le_bytes());
        }
        put(out, 32, &d.class);
        put(out, 34, &d.slot.to_le_bytes());
    }
    if let Some(serial) = error.serial {
        valid |= 1 << 3;
        put(out, 40, &serial.to_le_bytes());
    }
    if !error.capability.is_empty() {
        valid |= 1 << 4;
        put_dwords(out, 48, 60, error.capability);
    }
    if dvsec != 0 {
        valid |= 1 << 5;
    }
    if log != 0 {
        valid |= 1 << 6;
    }
    put(out, 108, &(dvsec as u16).to_le_bytes());
    put(out, 110, &(log as u16).to_le_bytes());
    put_dwords(out, CXL_PROTOCOL_HEADER, dvsec, error.dvsec);
    put_dwords(out, CXL_PROTOCOL_HEADER + dvsec, log, error.error_log);
    put(out, 0, &valid.to_le_bytes());
    Some(length)
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
        out.extend_from_slice(&entry_header(
            &entry.section,
            entry.severity,
            entry.data.len() as u32,
            entry.fru_text.as_ref(),
        ));
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
    let mut out = alloc::vec![0u8; 80];
    let mut validation = 0u64;
    let mut put = |bit: u32, at: usize, bytes: &[u8]| {
        validation |= 1 << bit;
        out[at..at + bytes.len()].copy_from_slice(bytes);
    };
    if let Some(status) = error.status {
        put(0, 8, &status.to_le_bytes());
    }
    if let Some(node) = error.node {
        put(3, 32, &node.to_le_bytes());
    }
    if let Some(card) = error.card {
        put(4, 34, &card.to_le_bytes());
    }
    if let Some(module) = error.module {
        put(5, 36, &module.to_le_bytes());
    }
    if let Some(kind) = error.kind {
        put(14, 72, &[kind]);
    }
    out[..8].copy_from_slice(&validation.to_le_bytes());
    out
}

/// A processor generic error section (UEFI N.2.4.1, 192 bytes) of an
/// x64 processor (CPUID leaf 1 EAX `version`, brand string `brand`, from
/// leaves 8000_0002h-4) of an unknown error type and operation. No
/// target, requestor or responder address: a caller with data it cannot
/// name as one keeps it in a section of its own.
pub fn processor_generic(version: u32, brand: &[u8; 48]) -> Vec<u8> {
    let mut out = alloc::vec![0u8; 192];
    // Processor type, ISA, error type, operation, CPU version, brand (the
    // vendors' 2CFh without its target address).
    out[..8].copy_from_slice(&0xcfu64.to_le_bytes());
    // IA32/x64, x64.
    out[8] = 0;
    out[9] = 2;
    out[16..20].copy_from_slice(&version.to_le_bytes());
    out[24..72].copy_from_slice(brand);
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

    /// A PCIe section as UEFI N.2.7 lays it out (and the AMD CPM fills
    /// it: validation EFh without a serial number).
    #[test]
    fn a_pcie_section_where_uefi_puts_its_fields() {
        let capability = [0x0142_0010u32; 15];
        let aer: [u32; 18] = core::array::from_fn(|n| n as u32);
        let section = pcie(&PcieError {
            port_type: Some(4),
            version: Some(2),
            command_status: Some(0x0010_0407),
            device: Some(PcieDevice {
                vendor: 0x1022,
                device: 0x1483,
                class: [0, 4, 6],
                function: 1,
                device_number: 1,
                segment: 0,
                primary_bus: 0,
                secondary_bus: 2,
                slot: 3 << 3,
            }),
            serial: None,
            bridge: Some(0x0002_0000),
            capability: &capability,
            aer: &aer,
        });
        assert_eq!(&section[..8], &0xefu64.to_le_bytes());
        assert_eq!(&section[8..16], &[4, 0, 0, 0, 2, 0, 0, 0]);
        assert_eq!(
            &section[24..40],
            &[
                0x22, 0x10, 0x83, 0x14, 0, 4, 6, 1, 1, 0, 0, 0, 2, 0x18, 0, 0
            ]
        );
        assert_eq!(&section[48..52], &[0, 0, 2, 0]);
        assert_eq!(&section[52..56], &0x0142_0010u32.to_le_bytes());
        // AER info: 96 bytes, the 18 dwords; root error status at +0x30.
        assert_eq!(&section[112 + 0x30..112 + 0x34], &12u32.to_le_bytes());
        assert_eq!(&section[180..184], &17u32.to_le_bytes());
        assert_eq!(&section[184..], &[0; 24]);
    }

    /// A GHES's block, in place: empty, one corrected entry, a second
    /// (multiple), an uncorrected one on top (worst severity), nothing
    /// once full; a block the OS cleared starts over.
    #[test]
    fn entries_go_into_a_status_block_in_place() {
        let mut block = [0u8; 20 + 2 * (72 + 208) + 100];
        let data = [0x5au8; 208];
        append(&mut block, &PCIE, Severity::Corrected, &data, None).unwrap();
        assert_eq!(&block[..4], &(0b10u32 | 1 << 4).to_le_bytes());
        assert_eq!(&block[12..16], &280u32.to_le_bytes());
        assert_eq!(&block[16..20], &2u32.to_le_bytes());
        assert_eq!(&block[20..36], &PCIE);
        assert_eq!(&block[40..42], &[0, 3]);
        assert_eq!(&block[44..48], &208u32.to_le_bytes());
        assert_eq!(block[92], 0x5a);
        append(&mut block, &PCIE, Severity::Recoverable, &data, None).unwrap();
        // Correctable and uncorrectable valid; two entries; recoverable.
        assert_eq!(&block[..4], &(0b11u32 | 2 << 4).to_le_bytes());
        assert_eq!(&block[12..16], &560u32.to_le_bytes());
        assert_eq!(&block[16..20], &0u32.to_le_bytes());
        let before = block;
        assert_eq!(
            append(&mut block, &PCIE, Severity::Fatal, &data, None),
            Err(Full)
        );
        assert_eq!(block, before);
        // The OS consumed it (block status 0): the next starts at 20.
        block[..4].copy_from_slice(&[0; 4]);
        append(&mut block, &CXL_PROTOCOL, Severity::Fatal, &[1; 8], None).unwrap();
        assert_eq!(&block[..4], &(1u32 | 1 << 4).to_le_bytes());
        assert_eq!(&block[12..16], &80u32.to_le_bytes());
        assert_eq!(&block[16..20], &1u32.to_le_bytes());
        assert_eq!(&block[20..36], &CXL_PROTOCOL);
        // A second uncorrectable: multiple.
        append(&mut block, &PCIE, Severity::Recoverable, &[0; 8], None).unwrap();
        assert_eq!(block[0], 0b0101 | 2 << 4);
        assert_eq!(&block[16..20], &1u32.to_le_bytes(), "fatal stays");
    }

    /// A CXL protocol section as Linux's `cxl_cper_sec_prot_err` reads
    /// it: a root port, its Flex Bus DVSEC and its RAS capability.
    #[test]
    fn a_cxl_protocol_section() {
        let mut out = [0xffu8; 300];
        let dvsec = [0x11u32; 5];
        let log = [0x22u32; 22];
        let length = cxl_protocol(
            &mut out,
            &CxlProtocolError {
                agent: CxlAgent::RootPort {
                    segment: 1,
                    bus: 0x80,
                    devfn: 0x0a,
                },
                device: Some(CxlDevice {
                    vendor: 0x1022,
                    device: 0x1483,
                    subsystem_vendor: 0,
                    subsystem: 0,
                    class: [4, 6],
                    slot: 0,
                }),
                serial: None,
                capability: &[],
                dvsec: &dvsec,
                error_log: &log,
            },
        )
        .unwrap();
        // The vendor's root port section: 0x128 bytes with its header.
        assert_eq!(length + ENTRY_HEADER, 0x128);
        assert_eq!(&out[..8], &0x67u64.to_le_bytes());
        assert_eq!(out[8], 5);
        assert_eq!(&out[9..16], &[0; 7]);
        // Function 2, device 1, bus 80h, segment 1.
        assert_eq!(&out[16..24], &[2, 1, 0x80, 1, 0, 0, 0, 0]);
        assert_eq!(&out[108..112], &[20, 0, 88, 0]);
        assert_eq!(&out[116..120], &0x11u32.to_le_bytes());
        assert_eq!(&out[136..140], &0x22u32.to_le_bytes());
        assert_eq!(out[length], 0xff, "nothing past it");
        assert_eq!(
            cxl_protocol(
                &mut [0; 100],
                &CxlProtocolError {
                    agent: CxlAgent::RchDownstreamPort { rcrb: 0xfe00_0000 },
                    device: None,
                    serial: None,
                    capability: &[],
                    dvsec: &[],
                    error_log: &log,
                }
            ),
            None
        );
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
