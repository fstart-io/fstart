//! A discrete TPM on the TIS/FIFO MMIO interface (TCG PC Client PTP 1.05
//! §6.3; locality 0 at `0xFED4_0000`): the probe, the `\_SB` device and,
//! for TPM 2.0, the TPM2 table (TCG ACPI 1.3, start method 6).

use alloc::vec::Vec;

use acpi_tables::tpm2::{PlatformClass, StartMethod, Tpm2};

/// Locality 0 of the TIS/FIFO interface and the span the device claims.
pub const TIS_BASE: u32 = 0xFED4_0000;
pub const TIS_SIZE: u32 = 0x5000;

const ACCESS: u32 = 0x00;
const INTF_CAPABILITY: u32 = 0x14;
const INTERFACE_ID: u32 = 0x30;
/// TPM_ACCESS.tpmRegValidSts.
const REG_VALID: u8 = 1 << 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TpmFamily {
    Tpm12,
    Tpm20,
}

/// Identify the TPM at [`TIS_BASE`] from `read32(offset)`: nothing when
/// TPM_ACCESS reads all ones or not valid; TPM 2.0 for the PTP FIFO
/// interface (TPM_INTERFACE_ID type 0) or InterfaceVersion 3 in
/// TPM_INTF_CAPABILITY; TPM 1.2 otherwise.
pub fn probe(read32: impl Fn(u32) -> u32) -> Option<TpmFamily> {
    let access = read32(ACCESS) as u8;
    if access == 0xFF || access & REG_VALID == 0 {
        return None;
    }
    let id = read32(INTERFACE_ID);
    let fifo = id != 0xFFFF_FFFF && id & 0xF == 0;
    let version = (read32(INTF_CAPABILITY) >> 28) & 7;
    Some(if fifo || version == 3 {
        TpmFamily::Tpm20
    } else {
        TpmFamily::Tpm12
    })
}

/// The TPM's ACPI description.
#[derive(Debug, Clone, Copy)]
pub struct TpmTisAcpi {
    pub family: TpmFamily,
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

    /// TPM 2.0: the TPM2 table (server class, FIFO, no event log). TPM 1.2
    /// needs no table without an event log.
    pub fn tables(&self) -> Vec<Vec<u8>> {
        match self.family {
            TpmFamily::Tpm20 => alloc::vec![crate::serialize(&Tpm2::new(
                crate::OEM_ID,
                crate::OEM_TABLE_ID,
                crate::OEM_REVISION,
                PlatformClass::Server,
                0,
                StartMethod::Mmio,
            ))],
            TpmFamily::Tpm12 => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_reads_the_interface() {
        let regs = |access: u32, caps: u32, id: u32| {
            move |off: u32| match off {
                ACCESS => access,
                INTF_CAPABILITY => caps,
                INTERFACE_ID => id,
                _ => 0xFFFF_FFFF,
            }
        };
        assert_eq!(probe(regs(0xFF, 0xFFFF_FFFF, 0xFFFF_FFFF)), None);
        assert_eq!(probe(regs(0x00, 0, 0)), None, "access not valid");
        assert_eq!(
            probe(regs(0x81, 0x3000_0000, 0x0000_0000)),
            Some(TpmFamily::Tpm20)
        );
        assert_eq!(
            probe(regs(0x81, 0x0000_0000, 0xFFFF_FFFF)),
            Some(TpmFamily::Tpm12)
        );
    }

    #[test]
    fn tpm2_table() {
        let t = TpmTisAcpi {
            family: TpmFamily::Tpm20,
        };
        let tables = t.tables();
        assert_eq!(&tables[0][..4], b"TPM2");
        assert_eq!(&tables[0][48..52], &6u32.to_le_bytes(), "start method");
        assert!(t.dsdt_aml().windows(8).any(|w| w == b"MSFT0101"));
        assert!(
            TpmTisAcpi {
                family: TpmFamily::Tpm12
            }
            .tables()
            .is_empty()
        );
    }
}
