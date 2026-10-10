//! X6 board DSDT glue: sleep/wake hooks, the UltraBase dock device driven
//! through the TCO mailbox of the board SMM handler, the EC wake GPE (GPIO8,
//! GPE 0x18 on ICH7 and ICH8 alike) and the complete H8 EC surface.
//!
//! The `_Q` mapping follows the vendor DSDT as in coreboot's X61 port:
//! `_Q18` is the Fn-F9 hotkey, `_Q37`/`_Q58` dock attach, `_Q50` undock.

extern crate alloc;

use alloc::vec::Vec;
use fstart_acpi_macros::acpi_dsl;

use crate::h8::H8Config;

/// Board DSDT fragments. `\_SB.PCI0.GFX0` must provide `INCB`/`DECB`.
pub fn dsdt_aml(h8: &H8Config, lpc_scope: &str) -> Vec<u8> {
    let mut out = Vec::new();

    // Root sleep/wake glue. Dock commands use the existing TCO mailbox,
    // not an unbacked AML SMIF variable or an I/O trap/GNVS channel.
    out.extend_from_slice(&acpi_dsl! {
        Scope("\\") {
            Method("_PTS", 1, NotSerialized) {
                #{const "\\_SB_.PCI0.LPCB.EC__.MUTE"}(1u32);
                #{const "\\_SB_.PCI0.LPCB.EC__.USBP"}(0u32);
                #{const "\\_SB_.PCI0.LPCB.EC__.RADI"}(0u32);
                #{const "\\_SB_.PCI0.LPCB.EC__.HKEY.MHKC"}(0u32);
            }
            Method("_WAK", 1, NotSerialized) {
                #{const "\\_SB_.PCI0.LPCB.EC__.HKEY.MHKC"}(1u32);
                #{const "\\_SB_.PCI0.LPCB.EC__.HKEY.WAKE"}(Arg0);
                Return(Package(0u32, 0u32));
            }
        }
    });

    // Dock: DLPC presence and the TCO mailbox to the SMM dock handler.
    out.extend_from_slice(&acpi_dsl! {
        Scope(#{const "\\_SB_"}) {
            OperationRegion("DLPC", SystemIO, 0x164Cu32, 0x01u32);
            Field("DLPC", ByteAcc, NoLock, Preserve) {
                , 3,
                DSTA, 1,
            }
            OperationRegion("TCOX", SystemIO, 0x0560u32, 0x20u32);
            Field("TCOX", ByteAcc, NoLock, Preserve) {
                Offset(0x02),
                TDIN, 8,
                TDOT, 8,
            }
            Device("DOCK") {
                Name("_HID", "ACPI0003");
                Name("_UID", 0u32);
                Name("_PCL", Package(#{const "\\_SB_"}));
                Method("_DCK", 1, Serialized) {
                    If (Arg0) {
                        TDIN = 1u32;
                    } Else {
                        TDIN = 2u32;
                    }
                    Return(TDOT);
                }
                Method("_PSR", 0, NotSerialized) {
                    Return(DSTA);
                }
                Method("_STA", 0, NotSerialized) {
                    Return(DSTA);
                }
            }
        }
    });

    // GPE routing: EC wake events (level-triggered GPIO8 wake path).
    out.extend_from_slice(&acpi_dsl! {
        Scope(#{const "\\_GPE"}) {
            Method("_L18", 0, NotSerialized) {
                Local0 = #{const "\\_SB_.PCI0.LPCB.EC__.WAKE"};
                If (Local0 & 0x04u32) {
                    Notify(#{const "\\_SB_.PCI0.LPCB.EC__.LID_"}, 0x02u32);
                }
                If (Local0 & 0x08u32) {
                    Notify(#{const "\\_SB_.DOCK"}, 0x03u32);
                    Notify(#{const "\\_SB_.PCI0.LPCB.EC__.SLPB"}, 0x02u32);
                }
                If (Local0 & 0x10u32) {
                    Notify(#{const "\\_SB_.PCI0.LPCB.EC__.SLPB"}, 0x02u32);
                }
                If (Local0 & 0x80u32) {
                    Notify(#{const "\\_SB_.PCI0.LPCB.EC__.SLPB"}, 0x02u32);
                }
            }
        }
    });

    // The complete H8 EC surface (EC device, batteries, thermal zones
    // with fan power resource, lid, AC, sleep button, HKEY hub, and the
    // PMH7/ECMM/ECGS/TWRI resource devices).
    let mut h8 = *h8;
    h8.has_bluetooth = super::bluetooth_present();
    let brightness = acpi_dsl! {
        Scope("\\") {
            Method("BRTU", 0, NotSerialized) { #{const "\\_SB_.PCI0.GFX0.INCB"}(); }
            Method("BRTD", 0, NotSerialized) { #{const "\\_SB_.PCI0.GFX0.DECB"}(); }
        }
    };
    out.extend(crate::h8_acpi::dsdt_aml_with_brightness(
        &h8,
        lpc_scope,
        &brightness,
    ));

    // Open the EC scope only after its Device declaration. This also
    // allows standalone ACPICA disassembly/recompilation without externals.
    out.extend_from_slice(&acpi_dsl! {
        Scope(#{const "\\_SB_.PCI0.LPCB.EC__"}) {
            Method("_Q18", 0, NotSerialized) { #{const "^HKEY.RHK_"}(0x09u32); }
            Method("_Q37", 0, NotSerialized) { Notify(#{const "\\_SB_.DOCK"}, 0u32); }
            Method("_Q50", 0, NotSerialized) { Notify(#{const "\\_SB_.DOCK"}, 3u32); }
            Method("_Q58", 0, NotSerialized) { Notify(#{const "\\_SB_.DOCK"}, 0u32); }
        }
    });

    out
}
