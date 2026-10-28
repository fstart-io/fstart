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

#[cfg(test)]
mod tests {
    #[test]
    fn builds() {
        let b = super::platform_osc();
        assert!(b.windows(4).any(|w| w == b"_OSC"));
    }
}
