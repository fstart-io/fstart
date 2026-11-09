//! ThinkPad X201 board DSDT (coreboot x201 `dsdt.asl`, `acpi/*.asl`): the
//! shared H8 glue, the Mini Dock and the X201T digitizer.

extern crate alloc;

use alloc::vec::Vec;
use fstart_acpi_macros::acpi_dsl;

use crate::h8::H8Config;
use crate::h8_acpi::ec;
use fstart_acpi::PcNamespace;

/// Board DSDT. `\_SB.PCI0.GFX0` provides `INCB`/`DECB` and the processor
/// AML `\CNOT`; `gpio_base` is the PCH GPIOBASE.
#[must_use]
pub fn dsdt_aml<N: PcNamespace>(h8: &H8Config, gpio_base: u16, digitizer: bool) -> Vec<u8> {
    let mut aml = crate::thinkpad::h8_dsdt_aml::<N>(h8);
    aml.extend(dock_aml::<N>(gpio_base));
    if digitizer {
        aml.extend(
            fstart_acpi::aml_linker::scope_vec(N::LPC.as_str(), &crate::wacom::digitizer_aml())
                .expect("X201 digitizer emission"),
        );
    }
    aml
}

/// coreboot x201 `acpi/dock.asl`: the dock follows EC `DKR1` and switches
/// all three EC dock links with the GPIO28 connect line. Only that GPIO
/// level bit is declared (GPIOBASE + 0x0c, pch.asl's `\GP28`).
fn dock_aml<N: PcNamespace>(gpio_base: u16) -> Vec<u8> {
    let gpio_level = u32::from(gpio_base) + 0x0c;
    let mut aml: Vec<u8> = acpi_dsl! {
        Scope("\\") {
            OperationRegion("GPLV", SystemIO, #{dword gpio_level}, 0x04u32);
            Field("GPLV", ByteAcc, NoLock, Preserve) {
                , 28,
                GP28, 1,
            }
        }
    }
    .into();
    aml.extend_from_slice(&acpi_dsl! {
        Scope(#{const path 4 ec::<N>()}) {
            Method("DKSE", 1, NotSerialized) {
                DKR1 = Arg0;
                DKR2 = Arg0;
                DKR3 = Arg0;
            }
        }
    });
    aml.extend_from_slice(&acpi_dsl! {
        Scope(#{const "\\_SB_"}) {
            Device("DOCK") {
                Name("_HID", "ACPI0003");
                Name("_UID", 0u32);
                Name("_PCL", Package(#{const "\\_SB_"}));
                Method("_DCK", 1, NotSerialized) {
                    GP28 = Arg0;
                    #{const path 5 ec::<N>().child("DKSE")}(Arg0);
                    Local1 = #{const path 5 ec::<N>().child("DKR1")};
                    Local0 = Arg0 ^ Local1;
                    Return(Local0);
                }
                Method("_STA", 0, NotSerialized) {
                    Local0 = #{const path 5 ec::<N>().child("DKR1")};
                    Return(Local0);
                }
            }
        }
    });
    aml.extend_from_slice(&acpi_dsl! {
        Scope(#{const path 4 ec::<N>()}) {
            Method("_Q18", 0, NotSerialized) { Notify(#{const "\\_SB_.DOCK"}, 3u32); }
            Method("_Q45", 0, NotSerialized) { Notify(#{const "\\_SB_.DOCK"}, 3u32); }
            Method("_Q50", 0, NotSerialized) { Notify(#{const "\\_SB_.DOCK"}, 3u32); }
            Method("_Q58", 0, NotSerialized) { Notify(#{const "\\_SB_.DOCK"}, 0u32); }
            Method("_Q37", 0, NotSerialized) { Notify(#{const "\\_SB_.DOCK"}, 0u32); }
        }
    });
    aml
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ec::EcPorts, h8::H8Resources};

    #[test]
    fn dock_and_glue_are_emitted() {
        let h8 = H8Config::new(
            H8Resources::new(EcPorts::new(0x62, 0x66), 0x1600, 0x15e0),
            0x11,
            "IBM0068",
        );
        let aml = dsdt_aml::<crate::h8_acpi::TestNamespace>(&h8, 0x480, false);
        let count = |name: &[u8]| aml.windows(name.len()).filter(|w| *w == name).count();
        assert_eq!(count(b"_DCK"), 1);
        assert_eq!(count(b"_Q37"), 1);
        assert_eq!(count(b"UCMS"), 1);
        assert_eq!(count(b"DTR_"), 0);
    }
}
