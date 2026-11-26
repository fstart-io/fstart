//! HEST (Hardware Error Source Table), ACPI 6.5 §18.3.2: the IA-32
//! architecture machine-check sources (types 0 and 1), the PCI Express
//! AER sources (types 6, 7, 8) and the generic hardware error sources
//! (GHES, types 9 and 10), which `acpi_tables::hest` does not have.
//!
//! The machine-check sources describe the CPU's MCA to the OS. Without the
//! firmware-first flag the OS owns the banks; the corrected source's error
//! threshold is the OS's thresholding limit (Linux:
//! `mce_get_apei_thr_limit`). An AER source with the firmware-first flag
//! says the firmware handles that class of PCIe errors and reports them
//! through a GHES: an error status block in memory the OS reads when the
//! source's notification (polled, SCI, NMI) says so.

use acpi_tables::sdt::Sdt;
use alloc::vec::Vec;

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
        out.extend_from_slice(&[
            self.number,
            self.clear_status_on_init as u8,
            self.status_format,
            0,
        ]);
        out.extend_from_slice(&self.control.to_le_bytes());
        out.extend_from_slice(&self.control_data.to_le_bytes());
        out.extend_from_slice(&self.status.to_le_bytes());
        out.extend_from_slice(&self.address.to_le_bytes());
        out.extend_from_slice(&self.misc.to_le_bytes());
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

/// How a source tells the OS of an error (ACPI 6.5 table 18.14).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notify {
    /// The OS polls every `ms`.
    Polled { ms: u32 },
    /// An SCI: the OS reads the source when the Hardware Error Device
    /// (`PNP0C33`) is notified (Linux: `ghes_notify_hed`).
    Sci,
    /// An NMI.
    Nmi,
}

impl Notify {
    /// The 28-byte hardware error notification structure.
    fn bytes(&self, out: &mut Vec<u8>) {
        let (kind, poll) = match *self {
            Self::Polled { ms } => (0u8, ms),
            Self::Sci => (3, 0),
            Self::Nmi => (4, 0),
        };
        out.extend_from_slice(&[kind, 28]);
        // Nothing configurable.
        out.extend_from_slice(&0u16.to_le_bytes());
        for value in [poll, 0, 0, 0, 0, 0] {
            out.extend_from_slice(&value.to_le_bytes());
        }
    }
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
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&source_id.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        // Enabled; one record preallocated, one section per record.
        out.extend_from_slice(&[flags, 1]);
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        match *self {
            Self::Exception {
                global_capability,
                global_control,
                ..
            } => {
                out.extend_from_slice(&global_capability.to_le_bytes());
                out.extend_from_slice(&global_control.to_le_bytes());
                out.push(banks.len() as u8);
                out.extend_from_slice(&[0; 7]);
            }
            Self::Corrected {
                poll_ms, threshold, ..
            } => {
                // Polled, the OS's thresholding limit.
                let mut notify = Vec::new();
                Notify::Polled { ms: poll_ms }.bytes(&mut notify);
                if let Some(value) = notify.get_mut(20..24) {
                    value.copy_from_slice(&threshold.to_le_bytes());
                }
                out.extend_from_slice(&notify);
                out.push(banks.len() as u8);
                out.extend_from_slice(&[0; 3]);
            }
        }
        banks.iter().for_each(|bank| bank.bytes(out));
    }
}

/// The AER registers a PCI Express AER source sets (ACPI 6.5 tables
/// 18.7-18.9), with the firmware-first flag the values firmware set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AerRegisters {
    pub device_control: u16,
    pub uncorrectable_mask: u32,
    pub uncorrectable_severity: u32,
    pub correctable_mask: u32,
    pub capabilities_control: u32,
}

/// Which functions a PCI Express AER source is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AerKind {
    /// Root ports (type 6), with their Root Error Command.
    RootPort { root_error_command: u32 },
    /// Endpoints (type 7).
    Device,
    /// PCIe-PCI bridges (type 8), with their secondary AER registers.
    Bridge {
        secondary_uncorrectable_mask: u32,
        secondary_uncorrectable_severity: u32,
        secondary_capabilities_control: u32,
    },
}

/// A PCI Express AER source: every function of its kind with
/// [`GLOBAL`], or one (`bus`, `device`, `function`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Aer {
    pub kind: AerKind,
    pub source_id: u16,
    pub flags: u8,
    pub bus: u32,
    pub device: u16,
    pub function: u16,
    pub registers: AerRegisters,
}

impl Aer {
    fn bytes(&self, out: &mut Vec<u8>) {
        let kind: u16 = match self.kind {
            AerKind::RootPort { .. } => 6,
            AerKind::Device => 7,
            AerKind::Bridge { .. } => 8,
        };
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&self.source_id.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        // Enabled; one record, sixteen sections (the AMD vendors').
        out.extend_from_slice(&[self.flags, 1]);
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&self.bus.to_le_bytes());
        out.extend_from_slice(&self.device.to_le_bytes());
        out.extend_from_slice(&self.function.to_le_bytes());
        out.extend_from_slice(&self.registers.device_control.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        let r = &self.registers;
        for value in [
            r.uncorrectable_mask,
            r.uncorrectable_severity,
            r.correctable_mask,
            r.capabilities_control,
        ] {
            out.extend_from_slice(&value.to_le_bytes());
        }
        match self.kind {
            AerKind::RootPort { root_error_command } => {
                out.extend_from_slice(&root_error_command.to_le_bytes());
            }
            AerKind::Device => {}
            AerKind::Bridge {
                secondary_uncorrectable_mask,
                secondary_uncorrectable_severity,
                secondary_capabilities_control,
            } => {
                for value in [
                    secondary_uncorrectable_mask,
                    secondary_uncorrectable_severity,
                    secondary_capabilities_control,
                ] {
                    out.extend_from_slice(&value.to_le_bytes());
                }
            }
        }
    }
}

/// The read acknowledge of a GHESv2 (ACPI 6.5 table 18.16): after it read
/// the block the OS writes `(value & preserve) | write` to the qword at
/// `address`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadAck {
    pub address: u64,
    pub preserve: u64,
    pub write: u64,
}

/// A generic hardware error source (type 9, type 10 with a read
/// acknowledge).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ghes {
    pub source_id: u16,
    /// The source (an AER source) it reports for; `0xFFFF` for none.
    pub related_source_id: u16,
    pub max_sections: u32,
    /// The address of the qword that holds the error status block's.
    pub status_address: u64,
    pub notify: Notify,
    /// The error status block's length.
    pub block_length: u32,
    pub read_ack: Option<ReadAck>,
}

/// A memory GAS for a qword at `address`.
fn qword_gas(address: u64, out: &mut Vec<u8>) {
    // System memory, 64 bits wide at bit 0, qword access.
    out.extend_from_slice(&[0, 64, 0, 4]);
    out.extend_from_slice(&address.to_le_bytes());
}

impl Ghes {
    fn bytes(&self, out: &mut Vec<u8>) {
        let kind: u16 = if self.read_ack.is_some() { 10 } else { 9 };
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&self.source_id.to_le_bytes());
        out.extend_from_slice(&self.related_source_id.to_le_bytes());
        // Flags none, enabled; one record; sections; raw data as long as
        // the block.
        out.extend_from_slice(&[0, 1]);
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&self.max_sections.to_le_bytes());
        out.extend_from_slice(&self.block_length.to_le_bytes());
        qword_gas(self.status_address, out);
        self.notify.bytes(out);
        out.extend_from_slice(&self.block_length.to_le_bytes());
        if let Some(ack) = self.read_ack {
            qword_gas(ack.address, out);
            out.extend_from_slice(&ack.preserve.to_le_bytes());
            out.extend_from_slice(&ack.write.to_le_bytes());
        }
    }
}

/// One error source of the HEST.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source<'a> {
    MachineCheck(MachineCheck, &'a [Bank]),
    Aer(Aer),
    Ghes(Ghes),
}

const HEADER: u32 = 40;

/// The HEST of `sources`.
pub fn table(
    oem_id: [u8; 6],
    oem_table_id: [u8; 8],
    oem_revision: u32,
    sources: &[Source<'_>],
) -> Vec<u8> {
    let mut sdt = Sdt::new(*b"HEST", HEADER, 1, oem_id, oem_table_id, oem_revision);
    sdt.write_u32(36, sources.len() as u32);
    let mut body = Vec::new();
    for source in sources {
        match source {
            Source::MachineCheck(source, banks) => source.bytes(banks, &mut body),
            Source::Aer(aer) => aer.bytes(&mut body),
            Source::Ghes(ghes) => ghes.bytes(&mut body),
        }
    }
    sdt.append_slice(&body);
    sdt.update_checksum();
    let mut bytes = Vec::new();
    crate::Aml::to_aml_bytes(&sdt, &mut bytes);
    bytes
}

/// The Hardware Error Device (`PNP0C33`, ACPI 6.5 §18.3.2.7.2) at `path`:
/// the OS reads its SCI-notified GHESs when it is notified (0x80).
pub fn hardware_error_device(path: &str) -> Vec<u8> {
    use acpi_tables::Aml;
    use acpi_tables::aml::{Device, EISAName, Name, Path};
    let hid = Name::new(Path::new("_HID"), &EISAName::new("PNP0C33"));
    let uid = Name::new(Path::new("_UID"), &0u8);
    let device = Device::new(Path::new(path), alloc::vec![&hid, &uid]);
    let mut bytes = Vec::new();
    device.to_aml_bytes(&mut bytes);
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
                Source::MachineCheck(
                    MachineCheck::Exception {
                        source_id: 1,
                        flags: 0,
                        global_capability: 0x120,
                        global_control: u64::MAX,
                    },
                    &banks,
                ),
                Source::MachineCheck(
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

    /// The AMD CPM's PCIe sources (GIGABYTE AmdPlatformRasWhDxe
    /// `0x18128`): root port, device and bridge AER, firmware-first and
    /// global, 0x94 bytes; a GHES and a GHESv2 at their ACPI offsets.
    #[test]
    fn aer_and_generic_sources_as_acpi_lays_them_out() {
        let registers = AerRegisters {
            device_control: 7,
            uncorrectable_mask: 0x10_0000,
            uncorrectable_severity: 0x57ef_6030,
            correctable_mask: 0x2000,
            capabilities_control: 0,
        };
        let aer = |kind, source_id| {
            Source::Aer(Aer {
                kind,
                source_id,
                flags: FIRMWARE_FIRST | GLOBAL,
                bus: 0,
                device: 0,
                function: 0,
                registers,
            })
        };
        let t = table(
            *b"FSTART",
            *b"FSTARTFW",
            1,
            &[
                aer(
                    AerKind::RootPort {
                        root_error_command: 0,
                    },
                    0x100,
                ),
                aer(AerKind::Device, 0x101),
                aer(
                    AerKind::Bridge {
                        secondary_uncorrectable_mask: 0x10_0000,
                        secondary_uncorrectable_severity: 0x57ef_6030,
                        secondary_capabilities_control: 0,
                    },
                    0x102,
                ),
                Source::Ghes(Ghes {
                    source_id: 0x200,
                    related_source_id: 0x100,
                    max_sections: 16,
                    status_address: 0x7600_0000,
                    notify: Notify::Sci,
                    block_length: 0x1000,
                    read_ack: None,
                }),
                Source::Ghes(Ghes {
                    source_id: 0x210,
                    related_source_id: 0xffff,
                    max_sections: 16,
                    status_address: 0x7600_0008,
                    notify: Notify::Nmi,
                    block_length: 0x1000,
                    read_ack: Some(ReadAck {
                        address: 0x7600_0010,
                        preserve: u64::MAX,
                        write: 1,
                    }),
                }),
            ],
        );
        let body = &t[40..];
        assert_eq!(body.len(), 0x94 + 0x40 + 0x5c);
        // The vendor's dwords 0..: 0x01000006, flags 3 enabled 1, ...
        assert_eq!(&body[..8], &[6, 0, 0, 1, 0, 0, 3, 1]);
        assert_eq!(&body[0x18..0x1a], &7u16.to_le_bytes());
        assert_eq!(&body[0x24..0x28], &0x2000u32.to_le_bytes());
        assert_eq!(&body[0x30..0x34], &0x0101_0007u32.to_le_bytes());
        assert_eq!(&body[0x5c..0x60], &0x0102_0008u32.to_le_bytes());
        let ghes = &body[0x94..0xd4];
        assert_eq!(&ghes[..8], &[9, 0, 0, 2, 0, 1, 0, 1]);
        assert_eq!(&ghes[0x14..0x18], &[0, 64, 0, 4]);
        assert_eq!(&ghes[0x18..0x20], &0x7600_0000u64.to_le_bytes());
        assert_eq!(&ghes[0x20..0x22], &[3, 28]);
        assert_eq!(&ghes[0x3c..0x40], &0x1000u32.to_le_bytes());
        let v2 = &body[0xd4..];
        assert_eq!(&v2[..2], &[10, 0]);
        assert_eq!(v2[0x20], 4);
        assert_eq!(&v2[0x44..0x4c], &0x7600_0010u64.to_le_bytes());
        assert_eq!(&v2[0x4c..0x54], &[0xff; 8]);
        assert_eq!(&v2[0x54..0x5c], &1u64.to_le_bytes());
        let hed = hardware_error_device("AERR");
        assert!(hed.windows(4).any(|w| w == b"AERR"));
        // EisaId("PNP0C33").
        assert!(hed.windows(4).any(|w| w == [0x41, 0xd0, 0x0c, 0x33]));
    }
}
