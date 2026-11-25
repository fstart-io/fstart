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

/// PCI host bridge `_OSC` control bits (CDW3, PCI Firmware 3.3 §4.5.1).
pub mod pci_control {
    pub const NATIVE_HOT_PLUG: u32 = 1 << 0;
    pub const SHPC_HOT_PLUG: u32 = 1 << 1;
    pub const PME: u32 = 1 << 2;
    pub const AER: u32 = 1 << 3;
    pub const PCIE_CAPABILITY: u32 = 1 << 4;
    pub const LTR: u32 = 1 << 5;
    pub const DPC: u32 = 1 << 7;
    /// What a firmware-first platform keeps: AER and DPC, whose errors its
    /// SMM handles (the AMD vendors' `_OSC`: `CDW3 & ~0x88`).
    pub const FIRMWARE_FIRST: u32 = AER | DPC;
}

/// CXL host bridge `_OSC` control bits (CDW5, CXL 3.1 §9.18.2).
pub mod cxl_control {
    /// CXL Memory Error Reporting Control.
    pub const MEMORY_ERROR_REPORTING: u32 = 1 << 0;
}

/// `_OSC` of a CXL host bridge (`ACPI0016`, CXL 3.1 §9.18.2): the PCI host
/// bridge UUID (CDW1-3) and the CXL one (CDW1-5: its CXL support in CDW4,
/// control in CDW5) are both recognized. Of the PCI controls the OS asks
/// for (CDW3) only those in `pci_controls` are granted, of the CXL ones
/// (CDW5) those in `cxl_controls`; one taken away sets "capabilities
/// masked" (CDW1 bit 4). The grant is the same for a query and a commit
/// (stateless). Another UUID sets "unrecognized UUID", a revision other
/// than 1 "unrecognized revision", a buffer shorter than its count says
/// "_OSC failure".
///
/// An OS-first platform passes every bit; a firmware-first one keeps
/// [`pci_control::FIRMWARE_FIRST`] and
/// [`cxl_control::MEMORY_ERROR_REPORTING`].
pub fn cxl_host_osc(pci_controls: u32, cxl_controls: u32) -> Vec<u8> {
    acpi_dsl! {
        Method("_OSC", 4, Serialized) {
            If (SizeOf(Arg3) < 4u8) { Name("BADR",Buffer(2u8,0u8,0u8,0u8)); Return(BADR); }
            CreateDwordField(Arg3, 0u8, "CDW1");
            If ((Arg0 != ToUUID("33db4d5b-1ff7-401c-9657-7441c03dd766"))
                && (Arg0 != ToUUID("68f2d50b-c469-4d8a-bd3d-941a103fd3fc"))) {
                CDW1 = CDW1 | 4u8;
                Return(Arg3);
            }
            If (Arg1 != 1u8) {
                CDW1 = CDW1 | 8u8;
                Return(Arg3);
            }
            If ((Arg2 < 3u8) || (SizeOf(Arg3) < (Arg2 * 4u8))) {
                CDW1 = CDW1 | 2u8;
                Return(Arg3);
            }
            CreateDwordField(Arg3, 8u8, "CDW3");
            Local0 = CDW3;
            CDW3 = CDW3 & #{dword pci_controls};
            If (Local0 != CDW3) { CDW1 = CDW1 | 16u8; }
            If ((Arg0 == ToUUID("68f2d50b-c469-4d8a-bd3d-941a103fd3fc")) && (Arg2 >= 5u8)) {
                CreateDwordField(Arg3, 16u8, "CDW5");
                Local1 = CDW5;
                CDW5 = CDW5 & #{dword cxl_controls};
                If (Local1 != CDW5) { CDW1 = CDW1 | 16u8; }
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cxl_host_osc_recognizes_the_pci_and_cxl_uuids() {
        let aml = cxl_host_osc(!0, !0);
        // ToUUID byte order of 68f2d50b-c469-4d8a-bd3d-941a103fd3fc.
        let cxl = [
            0x0b, 0xd5, 0xf2, 0x68, 0x69, 0xc4, 0x8a, 0x4d, 0xbd, 0x3d, 0x94, 0x1a, 0x10, 0x3f,
            0xd3, 0xfc,
        ];
        let pci = [
            0x5b, 0x4d, 0xdb, 0x33, 0xf7, 0x1f, 0x1c, 0x40, 0x96, 0x57, 0x74, 0x41, 0xc0, 0x3d,
            0xd7, 0x66,
        ];
        let has = |needle: &[u8]| aml.windows(needle.len()).any(|w| w == needle);
        assert!(has(&cxl) && has(&pci) && has(b"_OSC"));
    }
}
