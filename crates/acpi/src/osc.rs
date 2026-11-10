//! Platform-wide `\_SB._OSC` (ACPI 6.5 §6.2.11.2).

use alloc::vec::Vec;

use crate::acpi_dsl;

/// `\_SB._OSC` for an OS-first platform: every capability the OS offers
/// (CPPC, CPPC2, APEI, ...) is granted by returning its CDW2 unchanged; a
/// foreign UUID sets "unrecognized UUID", a revision other than 1
/// "unrecognized revision". Place it in `\_SB`.
pub fn platform_osc() -> Vec<u8> {
    acpi_dsl! {
        Method("_OSC", 4, NotSerialized) {
            CreateDwordField(Arg3, 0u8, "CDW1");
            If (Arg0 == ToUUID("0811b06e-4a27-44f9-8d60-3cbbc22e7b48")) {
                If (Arg1 != 1u8) {
                    CDW1 = CDW1 | 8u8;
                }
                Return(Arg3);
            }
            CDW1 = CDW1 | 4u8;
            Return(Arg3);
        }
    }
    .into()
}

/// PCI host bridge _OSC (PCI Firmware specification): grant only explicitly
/// supported native service control bits, never an unconditional pass-through.
/// This is stateless for query and commit; unknown UUID/revision or malformed
/// classic three-DWORD requests return protocol errors, not AML field faults.
pub fn pci_host_osc(controls: u32) -> Vec<u8> {
    acpi_dsl! {
        Method("_OSC", 4, Serialized) {
            If (SizeOf(Arg3) < 4u8) { Name("BADR",Buffer(2u8,0u8,0u8,0u8)); Return(BADR); }
            CreateDwordField(Arg3,0u8,"CDW1");
            If (Arg0 != ToUUID("33db4d5b-1ff7-401c-9657-7441c03dd766")) {
                CDW1 = CDW1 | 4u8; Return(Arg3);
            }
            If (Arg1 != 1u8) { CDW1 = CDW1 | 8u8; Return(Arg3); }
            If ((Arg2 != 3u8) || (SizeOf(Arg3) < 12u8)) {
                CDW1 = CDW1 | 2u8; Return(Arg3);
            }
            CreateDwordField(Arg3,8u8,"CDW3");
            Local0 = CDW3;
            CDW3 = CDW3 & #{dword controls};
            If (Local0 != CDW3) { CDW1 = CDW1 | 16u8; }
            Return(Arg3);
        }
    }
    .into()
}
