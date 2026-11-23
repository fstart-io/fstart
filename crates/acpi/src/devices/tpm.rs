//! A discrete TPM on the TIS/FIFO MMIO interface (TCG PC Client PTP 1.07
//! §6.3; locality 0 at `0xFED4_0000`): the `\_SB` device and the table that
//! points the OS at the TCG event log: `TPM2` (start method 6) for TPM 2.0,
//! `TCPA` (client or server class, TCG ACPI 1.5 §3) for TPM 1.2. The `TPM2`
//! table has the TCG ACPI 1.3 revision 4 layout (76 bytes, LAML at 64, LASA
//! at 68) that Linux and ACPICA parse, deliberately not 1.5's revision 6.
//! The interface itself is driven by `fstart-driver-tpm`.

use alloc::vec::Vec;

use acpi_tables::gas::{AccessSize, AddressSpace, GAS};
use acpi_tables::tpm2::{PlatformClass, StartMethod, Tpm2, TpmClient1_2, TpmServer1_2};

/// Locality 0 of the TIS/FIFO interface and the span the device claims.
pub const TIS_BASE: u32 = 0xFED4_0000;
pub const TIS_SIZE: u32 = 0x5000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TpmFamily {
    Tpm12,
    Tpm20,
}

/// The pre-boot TCG event log area (LASA and LAML).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogArea {
    pub base: u64,
    pub size: u32,
}

/// The TPM's ACPI description.
#[derive(Debug, Clone, Copy)]
pub struct TpmTisAcpi {
    pub family: TpmFamily,
    /// Server platforms use the server `TCPA` layout and class.
    pub server: bool,
    /// Where firmware left the event log; `None` writes no log pointer
    /// (TPM 2.0) or no table (TPM 1.2).
    pub log: Option<LogArea>,
}

/// Optional interrupt resource for a TIS TPM `_CRS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TpmIrq {
    pub vector: u32,
    pub edge: bool,
    pub active_low: bool,
}

/// Optional board-specific additions to the `Device(TPM)` DSDT node.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TpmTisOptions {
    /// Override the default `_UID` (`1`).
    pub uid: Option<u32>,
    /// Emit `_CID` equal to `_HID` for TPM 1.2 (TPM 2.0 always emits `_CID`).
    pub emit_cid: bool,
    /// Emit an explicit `Name("_STA", 0x0F)`.
    pub emit_sta: bool,
    /// Optional legacy I/O port range `(base, length)` in `_CRS`.
    pub io_port: Option<(u16, u8)>,
    /// Optional interrupt resource in `_CRS`.
    pub irq: Option<TpmIrq>,
}

impl TpmTisAcpi {
    /// `Device(TPM)` for `\_SB`: `MSFT0101` (TPM 2.0) or `PNP0C31`.
    pub fn dsdt_aml(&self) -> Vec<u8> {
        self.dsdt_aml_with(TpmTisOptions::default())
    }

    /// `Device(TPM)` with optional board-specific `_UID`, `_CID`, `_STA`, I/O
    /// port range, and interrupt descriptors.
    pub fn dsdt_aml_with(&self, options: TpmTisOptions) -> Vec<u8> {
        use crate::aml_linker::device_vec;
        use acpi_tables::Aml;
        use acpi_tables::aml::{IO, Interrupt, Memory32Fixed, Name, ResourceTemplate};

        let mut body: Vec<u8> = match self.family {
            TpmFamily::Tpm20 => fstart_acpi_macros::acpi_dsl! {
                Name("_HID", "MSFT0101");
                Name("_CID", "MSFT0101");
            }
            .into(),
            TpmFamily::Tpm12 if options.emit_cid => fstart_acpi_macros::acpi_dsl! {
                Name("_HID", EisaId("PNP0C31"));
                Name("_CID", EisaId("PNP0C31"));
            }
            .into(),
            TpmFamily::Tpm12 => fstart_acpi_macros::acpi_dsl! {
                Name("_HID", EisaId("PNP0C31"));
            }
            .into(),
        };
        if let Some(uid) = options.uid {
            Name::new("_UID".into(), &uid).to_aml_bytes(&mut body);
        } else {
            body.extend_from_slice(&fstart_acpi_macros::acpi_dsl! { Name("_UID", 1u8); });
        }
        if options.emit_sta {
            body.extend_from_slice(&fstart_acpi_macros::acpi_dsl! { Name("_STA", 0x0Fu8); });
        }
        let mem = Memory32Fixed::new(true, TIS_BASE, TIS_SIZE);
        let io = options
            .io_port
            .map(|(port, len)| IO::new(port, port, 1, len));
        let irq = options
            .irq
            .map(|i| Interrupt::new(true, i.edge, i.active_low, true, i.vector));
        let mut resources: Vec<&dyn Aml> = alloc::vec![&mem];
        if let Some(ref io) = io {
            resources.push(io);
        }
        if let Some(ref irq) = irq {
            resources.push(irq);
        }
        Name::new("_CRS".into(), &ResourceTemplate::new(resources)).to_aml_bytes(&mut body);
        device_vec("TPM", &body).expect("TPM device")
    }

    /// TPM 2.0: the `TPM2` table, with the log area when there is a log.
    /// TPM 1.2: the `TCPA` table, which exists to point at the log.
    pub fn tables(&self) -> Vec<Vec<u8>> {
        let class = if self.server {
            PlatformClass::Server
        } else {
            PlatformClass::Client
        };
        match (self.family, self.log) {
            (TpmFamily::Tpm20, log) => {
                let mut table = Tpm2::new(
                    crate::OEM_ID,
                    crate::OEM_TABLE_ID,
                    crate::OEM_REVISION,
                    class,
                    0,
                    StartMethod::Mmio,
                );
                if let Some(log) = log {
                    table.set_log_area(log.size, log.base);
                }
                let mut bytes = crate::serialize(&table);
                // TCG ACPI 1.3 §9: revision 4 is the layout with LAML and
                // LASA; acpi_tables writes 1.
                bytes[8] = 4;
                bytes[9] = 0;
                bytes[9] = 0u8.wrapping_sub(bytes.iter().fold(0u8, |a, b| a.wrapping_add(*b)));
                alloc::vec![bytes]
            }
            (TpmFamily::Tpm12, None) => Vec::new(),
            (TpmFamily::Tpm12, Some(log)) if self.server => {
                alloc::vec![crate::serialize(
                    &TpmServer1_2::new(crate::OEM_ID, crate::OEM_TABLE_ID, crate::OEM_REVISION)
                        .log_area(u64::from(log.size), log.base)
                        .base_addr(GAS::new(
                            AddressSpace::SystemMemory,
                            8,
                            0,
                            AccessSize::ByteAccess,
                            u64::from(TIS_BASE),
                        ))
                )]
            }
            (TpmFamily::Tpm12, Some(log)) => alloc::vec![crate::serialize(&TpmClient1_2::new(
                crate::OEM_ID,
                crate::OEM_TABLE_ID,
                crate::OEM_REVISION,
                log.size,
                log.base,
            ))],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: LogArea = LogArea {
        base: 0x7560_0000,
        size: 0x1_0000,
    };

    fn checksum_ok(table: &[u8]) -> bool {
        table.iter().fold(0u8, |a, b| a.wrapping_add(*b)) == 0
    }

    #[test]
    fn tpm2_table() {
        let t = TpmTisAcpi {
            family: TpmFamily::Tpm20,
            server: true,
            log: Some(LOG),
        };
        let tables = t.tables();
        let table = &tables[0];
        assert_eq!(&table[..4], b"TPM2");
        assert_eq!(table.len(), 76, "with LAML and LASA");
        assert_eq!(table[8], 4, "revision of the LAML/LASA layout");
        assert_eq!(&table[36..38], &1u16.to_le_bytes(), "server class");
        assert_eq!(&table[48..52], &6u32.to_le_bytes(), "start method");
        assert_eq!(&table[64..68], &LOG.size.to_le_bytes());
        assert_eq!(&table[68..76], &LOG.base.to_le_bytes());
        assert!(checksum_ok(table));
        assert!(t.dsdt_aml().windows(8).any(|w| w == b"MSFT0101"));
    }

    #[test]
    fn tcpa_tables() {
        let server = TpmTisAcpi {
            family: TpmFamily::Tpm12,
            server: true,
            log: Some(LOG),
        }
        .tables();
        assert_eq!(&server[0][..4], b"TCPA");
        assert_eq!(server[0].len(), 100);
        assert_eq!(&server[0][40..48], &u64::from(LOG.size).to_le_bytes());
        assert_eq!(&server[0][48..56], &LOG.base.to_le_bytes());
        assert!(checksum_ok(&server[0]));
        let client = TpmTisAcpi {
            family: TpmFamily::Tpm12,
            server: false,
            log: Some(LOG),
        }
        .tables();
        assert_eq!(client[0].len(), 50);
        assert!(checksum_ok(&client[0]));
        assert!(
            TpmTisAcpi {
                family: TpmFamily::Tpm12,
                server: true,
                log: None
            }
            .tables()
            .is_empty()
        );
    }

    #[test]
    fn dsdt_options_emit_uid_cid_sta_io_and_irq() {
        let tpm = TpmTisAcpi {
            family: TpmFamily::Tpm12,
            server: false,
            log: None,
        };
        let aml = tpm.dsdt_aml_with(TpmTisOptions {
            uid: Some(0x89e3_7d9c),
            emit_cid: true,
            emit_sta: true,
            io_port: Some((0x0c31, 2)),
            irq: Some(TpmIrq {
                vector: 5,
                edge: false,
                active_low: true,
            }),
        });
        assert!(aml.windows(4).any(|w| w == b"_CID"));
        assert!(aml.windows(4).any(|w| w == b"_STA"));
        assert!(aml.windows(4).any(|w| w == 0x89e3_7d9cu32.to_le_bytes()));
        assert!(aml.windows(2).any(|w| w == 0x0c31u16.to_le_bytes()));
        assert!(aml.windows(4).any(|w| w == 5u32.to_le_bytes()));
    }
}
