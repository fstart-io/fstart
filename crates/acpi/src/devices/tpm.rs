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

impl TpmTisAcpi {
    /// `Device(TPM)` for `\_SB`: `MSFT0101` (TPM 2.0) or `PNP0C31`.
    pub fn dsdt_aml(&self) -> Vec<u8> {
        match self.family {
            TpmFamily::Tpm20 => fstart_acpi_macros::acpi_dsl! {
                Device("TPM") {
                    Name("_HID", "MSFT0101");
                    Name("_CID", "MSFT0101");
                    Name("_UID", 1u8);
                    Name("_CRS", ResourceTemplate {
                        Memory32Fixed(ReadWrite, #{const dword TIS_BASE}, #{const dword TIS_SIZE});
                    });
                }
            }
            .into(),
            TpmFamily::Tpm12 => fstart_acpi_macros::acpi_dsl! {
                Device("TPM") {
                    Name("_HID", EisaId("PNP0C31"));
                    Name("_UID", 1u8);
                    Name("_CRS", ResourceTemplate {
                        Memory32Fixed(ReadWrite, #{const dword TIS_BASE}, #{const dword TIS_SIZE});
                    });
                }
            }
            .into(),
        }
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
}
