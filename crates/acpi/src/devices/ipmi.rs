//! IPMI KCS system interface: the `IPI0001` device and the SPMI table.

use alloc::vec::Vec;

use acpi_tables::Aml;
use acpi_tables::sdt::Sdt;

/// A BMC's KCS interface in system I/O space.
#[derive(Debug, Clone, Copy)]
pub struct IpmiKcsAcpi {
    /// Data register port; status/command is `port + spacing`.
    pub port: u16,
    /// Register spacing in bytes: 1, 4 or 16.
    pub spacing: u8,
    /// `_UID` of the device, repeated in the SPMI table.
    pub uid: u8,
    /// IPMI specification revision in BCD (0x0200 for IPMI 2.0).
    pub spec_revision: u16,
}

/// SPMI interface type: Keyboard Controller Style.
const INTERFACE_KCS: u8 = 1;
/// SPMI revision 5 (IPMI 2.0).
const SPMI_REVISION: u8 = 5;
const SPMI_LEN: u32 = 65;

impl IpmiKcsAcpi {
    /// `Device(SPMI)`, for the scope of the bus that decodes the ports (LPC).
    pub fn dsdt_aml(&self) -> Vec<u8> {
        let data = u64::from(self.port);
        let status = data + u64::from(self.spacing);
        let uid = u64::from(self.uid);
        let revision = u64::from(self.spec_revision);
        fstart_acpi_macros::acpi_dsl! {
            Device("SPMI") {
                Name("_HID", "IPI0001");
                Name("_UID", #{byte uid});
                Method("_STA", 0, NotSerialized) { Return(0x0Fu8); }
                Name("_CRS", ResourceTemplate {
                    IO(#{word data}, #{word data}, 0x01u8, 0x01u8);
                    IO(#{word status}, #{word status}, 0x01u8, 0x01u8);
                });
                // Interface type and IPMI revision.
                Method("_IFT", 0, NotSerialized) { Return(0x01u8); }
                Method("_SRV", 0, NotSerialized) { Return(#{word revision}); }
            }
        }
        .into()
    }

    /// The Service Processor Management Interface table.
    /// `\_PTS`, `\_WAK` and `\_SB._INI` reporting the system sleep state
    /// to the BMC through a software SMI: `sw_smi_base + Sx` on the SMI
    /// command port `smi_cmd` before sleeping, `sw_smi_base` (S0) at boot
    /// and on wake. The SMM handler turns them into Set ACPI Power State
    /// (AMI's BmcAcpi convention; `fstart_driver_ipmi::sleep_power_states`).
    /// These own `\_PTS`/`\_WAK`.
    pub fn sleep_notify_aml(smi_cmd: u16, sw_smi_base: u8) -> Vec<u8> {
        let port = u64::from(smi_cmd);
        let base = u64::from(sw_smi_base);
        fstart_acpi_macros::acpi_dsl! {
            Scope("\\") {
                OperationRegion("BSMP", SystemIO, #{word port}, 1u8);
                Field("BSMP", ByteAcc, NoLock, Preserve) {
                    BSMC, 8,
                }
                Method("_PTS", 1, NotSerialized) {
                    If (Arg0 != 0u8) {
                        BSMC = Arg0 + #{byte base};
                    }
                }
                Method("_WAK", 1, NotSerialized) {
                    BSMC = #{byte base};
                    Return(Package(0u8, 0u8));
                }
            }
            Scope("\\_SB_") {
                Method("_INI", 0, NotSerialized) {
                    BSMC = #{byte base};
                }
            }
        }
        .into()
    }

    pub fn spmi(&self) -> Vec<u8> {
        let mut spmi = Sdt::new(
            *b"SPMI",
            SPMI_LEN,
            SPMI_REVISION,
            crate::OEM_ID,
            crate::OEM_TABLE_ID,
            crate::OEM_REVISION,
        );
        spmi.write_u8(36, INTERFACE_KCS);
        // Reserved, must be 1.
        spmi.write_u8(37, 1);
        spmi.write_u16(38, self.spec_revision);
        // No SCI/GPE or APIC interrupt (interrupt type, GPE, PCI flag, GSI
        // stay 0): the OS polls.
        // Base address: GAS in system I/O, byte access; the bit offset holds
        // the register spacing.
        spmi.write_u8(48, 1);
        spmi.write_u8(49, 8);
        spmi.write_u8(50, self.spacing.saturating_mul(8));
        spmi.write_u8(51, 1);
        spmi.write_u64(52, u64::from(self.port));
        spmi.write_u32(60, u32::from(self.uid));
        spmi.update_checksum();
        let mut bytes = Vec::new();
        spmi.to_aml_bytes(&mut bytes);
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KCS: IpmiKcsAcpi = IpmiKcsAcpi {
        port: 0xca2,
        spacing: 1,
        uid: 0,
        spec_revision: 0x0200,
    };

    #[test]
    fn spmi_describes_the_kcs_ports() {
        let spmi = KCS.spmi();
        assert_eq!(spmi.len(), SPMI_LEN as usize);
        assert_eq!(&spmi[..4], b"SPMI");
        assert_eq!(spmi.iter().fold(0u8, |a, &b| a.wrapping_add(b)), 0);
        assert_eq!(&spmi[36..40], &[1, 1, 0x00, 0x02]);
        assert_eq!(&spmi[48..60], &[1, 8, 8, 1, 0xa2, 0x0c, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn device_claims_data_and_status_ports() {
        let aml = KCS.dsdt_aml();
        for port in [0xca2u16, 0xca3] {
            let [lo, hi] = port.to_le_bytes();
            assert!(aml.windows(8).any(|w| w == [0x47, 1, lo, hi, lo, hi, 1, 1]));
        }
    }
}
