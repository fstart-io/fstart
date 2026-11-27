//! HEST (Hardware Error Source Table), ACPI 6.5 §18.3.2: the IA-32
//! architecture machine-check sources (types 0 and 1), which
//! `acpi_tables::hest` does not have.
//!
//! These describe the CPU's MCA to the OS. Without the firmware-first
//! flag the OS owns the banks; the corrected source's error threshold is
//! the OS's thresholding limit (Linux: `mce_get_apei_thr_limit`).

use acpi_tables::sdt::Sdt;
use alloc::vec::Vec;
use zerocopy::little_endian::{U16, U32, U64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

#[repr(C)]
#[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
struct MceBankWire {
    number: u8,
    clear_status_on_init: u8,
    status_format: u8,
    reserved: u8,
    control: U32,
    control_data: U64,
    status: U32,
    address: U32,
    misc: U32,
}

#[repr(C)]
#[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
struct HestSourcePrefix {
    kind: U16,
    source_id: U16,
    related_source_id: U16,
    flags: u8,
    enabled: u8,
    records_to_preallocate: U32,
    max_sections_per_record: U32,
}

#[repr(C)]
#[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
struct MceBodyWire {
    global_capability: U64,
    global_control: U64,
    num_banks: u8,
    reserved: [u8; 7],
}

#[repr(C)]
#[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
struct ErrorNotificationWire {
    kind: u8,
    length: u8,
    config_write_enable: U16,
    poll_interval_ms: U32,
    vector: U32,
    switch_threshold_value: U32,
    switch_threshold_window: U32,
    error_threshold_value: U32,
    error_threshold_window: U32,
}

#[repr(C)]
#[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
struct CmcBodyWire {
    notification: ErrorNotificationWire,
    num_banks: u8,
    reserved: [u8; 3],
}

const _: () = assert!(core::mem::size_of::<MceBankWire>() == 28);
const _: () = assert!(core::mem::size_of::<HestSourcePrefix>() == 16);
const _: () = assert!(core::mem::size_of::<MceBodyWire>() == 24);
const _: () = assert!(core::mem::size_of::<ErrorNotificationWire>() == 28);
const _: () = assert!(core::mem::size_of::<CmcBodyWire>() == 32);

/// Source flags (ACPI 6.5 table 18.4).
pub const FIRMWARE_FIRST: u8 = 1 << 0;
pub const GLOBAL: u8 = 1 << 1;
pub const GHES_ASSIST: u8 = 1 << 2;

/// A machine-check bank (ACPI 6.5 table 18.6): its MSRs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bank {
    pub number: u8,
    pub clear_status_on_init: bool,
    /// 0 IA-32 MCA, 1 Intel 64 MCA, 2 AMD64 MCA.
    pub status_format: u8,
    pub control: u32,
    /// Written to the control register at initialization.
    pub control_data: u64,
    pub status: u32,
    pub address: u32,
    pub misc: u32,
}

impl Bank {
    /// An AMD64 SMCA bank: its MSRs at `C000_2000h + 16 * number`, every
    /// error enabled.
    pub const fn smca(number: u8) -> Self {
        let base = 0xc000_2000 + 16 * number as u32;
        Self {
            number,
            clear_status_on_init: false,
            status_format: 2,
            control: base,
            control_data: u64::MAX,
            status: base + 1,
            address: base + 2,
            misc: base + 3,
        }
    }

    fn bytes(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(
            MceBankWire {
                number: self.number,
                clear_status_on_init: self.clear_status_on_init as u8,
                status_format: self.status_format,
                reserved: 0,
                control: U32::new(self.control),
                control_data: U64::new(self.control_data),
                status: U32::new(self.status),
                address: U32::new(self.address),
                misc: U32::new(self.misc),
            }
            .as_bytes(),
        );
    }
}

/// An IA-32 machine-check source: the exception (type 0) or the
/// corrected-error interrupt (type 1, polled every `poll_ms` with the
/// OS's thresholding limit `threshold`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineCheck {
    Exception {
        source_id: u16,
        flags: u8,
        global_capability: u64,
        global_control: u64,
    },
    Corrected {
        source_id: u16,
        flags: u8,
        poll_ms: u32,
        threshold: u32,
    },
}

impl MachineCheck {
    fn bytes(&self, banks: &[Bank], out: &mut Vec<u8>) {
        let (kind, source_id, flags) = match *self {
            Self::Exception {
                source_id, flags, ..
            } => (0u16, source_id, flags),
            Self::Corrected {
                source_id, flags, ..
            } => (1u16, source_id, flags),
        };
        // Enabled; one record preallocated, one section per record.
        out.extend_from_slice(
            HestSourcePrefix {
                kind: U16::new(kind),
                source_id: U16::new(source_id),
                related_source_id: U16::ZERO,
                flags,
                enabled: 1,
                records_to_preallocate: U32::new(1),
                max_sections_per_record: U32::new(1),
            }
            .as_bytes(),
        );
        match *self {
            Self::Exception {
                global_capability,
                global_control,
                ..
            } => {
                out.extend_from_slice(
                    MceBodyWire {
                        global_capability: U64::new(global_capability),
                        global_control: U64::new(global_control),
                        num_banks: banks.len() as u8,
                        reserved: [0; 7],
                    }
                    .as_bytes(),
                );
            }
            Self::Corrected {
                poll_ms, threshold, ..
            } => {
                // Hardware error notification (ACPI 6.5 table 18.14):
                // polled, 28 bytes, nothing configurable.
                out.extend_from_slice(
                    CmcBodyWire {
                        notification: ErrorNotificationWire {
                            kind: 0,
                            length: core::mem::size_of::<ErrorNotificationWire>() as u8,
                            config_write_enable: U16::ZERO,
                            poll_interval_ms: U32::new(poll_ms),
                            vector: U32::ZERO,
                            switch_threshold_value: U32::ZERO,
                            switch_threshold_window: U32::ZERO,
                            error_threshold_value: U32::new(threshold),
                            error_threshold_window: U32::ZERO,
                        },
                        num_banks: banks.len() as u8,
                        reserved: [0; 3],
                    }
                    .as_bytes(),
                );
            }
        }
        banks.iter().for_each(|bank| bank.bytes(out));
    }
}

const HEADER: u32 = 40;

/// The HEST: `sources`, each with `banks`.
pub fn table(
    oem_id: [u8; 6],
    oem_table_id: [u8; 8],
    oem_revision: u32,
    sources: &[(MachineCheck, &[Bank])],
) -> Vec<u8> {
    let mut sdt = Sdt::new(*b"HEST", HEADER, 1, oem_id, oem_table_id, oem_revision);
    sdt.write_u32(36, sources.len() as u32);
    let mut body = Vec::new();
    for (source, banks) in sources {
        source.bytes(banks, &mut body);
    }
    sdt.append_slice(&body);
    sdt.update_checksum();
    let mut bytes = Vec::new();
    crate::Aml::to_aml_bytes(&sdt, &mut bytes);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_check_sources_as_acpi_lays_them_out() {
        let banks = [Bank::smca(0), Bank::smca(1)];
        let t = table(
            *b"FSTART",
            *b"FSTARTFW",
            1,
            &[
                (
                    MachineCheck::Exception {
                        source_id: 1,
                        flags: 0,
                        global_capability: 0x120,
                        global_control: u64::MAX,
                    },
                    &banks,
                ),
                (
                    MachineCheck::Corrected {
                        source_id: 2,
                        flags: 0,
                        poll_ms: 5000,
                        threshold: 10,
                    },
                    &banks,
                ),
            ],
        );
        assert_eq!(&t[..4], b"HEST");
        assert_eq!(t.iter().fold(0u8, |a, b| a.wrapping_add(*b)), 0);
        assert_eq!(u32::from_le_bytes(t[36..40].try_into().unwrap()), 2);
        // MCE: 40 bytes and 28 per bank, as the vendor's HEST (S8040).
        let mce = &t[40..40 + 40 + 56];
        assert_eq!(&mce[..2], &[0, 0]);
        assert_eq!(mce[32], 2);
        assert_eq!(&mce[40..44], &[0, 0, 2, 0]);
        assert_eq!(&mce[44..48], &0xc000_2000u32.to_le_bytes());
        assert_eq!(&mce[56..60], &0xc000_2001u32.to_le_bytes());
        // CMC: 48 bytes and the banks; polled every 5 s, threshold 10.
        let cmc = &t[136..];
        assert_eq!(cmc.len(), 48 + 56);
        assert_eq!(&cmc[..4], &[1, 0, 2, 0]);
        assert_eq!(&cmc[16..18], &[0, 28]);
        assert_eq!(u32::from_le_bytes(cmc[20..24].try_into().unwrap()), 5000);
        assert_eq!(u32::from_le_bytes(cmc[36..40].try_into().unwrap()), 10);
        assert_eq!(cmc[44], 2);
        assert_eq!(t.len(), 40 + 96 + 104);
    }
}
