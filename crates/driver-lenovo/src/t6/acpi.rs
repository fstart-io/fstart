//! T6 ACPI policy. Dock requests use the implemented TCO SMM mailbox rather
//! than copying coreboot's GNVS/I/O-trap transport. ATI brightness is omitted.
extern crate alloc;
use crate::h8::H8Config;
use alloc::vec::Vec;
use fstart_acpi_macros::acpi_dsl;

pub fn dsdt_aml(h8: &H8Config, lpc_scope: &str, native_brightness: bool) -> Vec<u8> {
    let mut out = Vec::new();
    let status_command = super::dock::SMI_DOCK_STATUS;
    // Keep PnP index/data transactions firmware-owned. Both AML operations
    // share one serialized mailbox transaction so they cannot mix replies.
    out.extend(Vec::<u8>::from(acpi_dsl! {
        Scope("\\") {
            Method("_PTS", 1, NotSerialized) {
                #{const "\\_SB_.PCI0.LPCB.EC__.MUTE"}(1u32);
                #{const "\\_SB_.PCI0.LPCB.EC__.USBP"}(0u32);
                #{const "\\_SB_.PCI0.LPCB.EC__.RADI"}(0u32);
            }
            Method("_WAK", 1, NotSerialized) {
                #{const "\\_SB_.PCI0.LPCB.EC__.HKEY.WAKE"}(Arg0);
                Return(Package(0u32, 0u32));
            }
        }
        Scope(#{const "\\_SB_"}) {
            OperationRegion("TCOX", SystemIO, 0x0560u32, 0x20u32);
            Field("TCOX", ByteAcc, NoLock, Preserve) {
                Offset(2), TDIN, 8, TDOT, 8,
            }
            Device("DOCK") {
                Name("_HID", "ACPI0003");
                Name("_UID", 0u32);
                Name("_PCL", Package(#{const "\\_SB_"}));
                Method("SMIC", 1, Serialized) {
                    TDIN = Arg0;
                    Return(TDOT);
                }
                Method("_DCK", 1, Serialized) {
                    If (Arg0) { Sleep(250u32); Return(SMIC(1u32)); }
                    Return(SMIC(2u32));
                }
                Method("_STA", 0, NotSerialized) {
                    Return(SMIC(#{byte status_command}));
                }
            }
        }
        Scope(#{const "\\_GPE"}) {
            Method("_L18", 0, NotSerialized) {
                Local0 = #{const "\\_SB_.PCI0.LPCB.EC__.WAKE"};
                Increment(Local0);
            }
        }
    }));
    let brightness = if native_brightness {
        acpi_dsl! { Scope("\\") {
            Method("BRTU", 0, NotSerialized) { #{const "\\_SB_.PCI0.GFX0.INCB"}(); }
            Method("BRTD", 0, NotSerialized) { #{const "\\_SB_.PCI0.GFX0.DECB"}(); }
        }}
        .to_vec()
    } else {
        acpi_dsl! { Scope("\\") {
            Method("BRTU", 0, NotSerialized) {}
            Method("BRTD", 0, NotSerialized) {}
        }}
        .to_vec()
    };
    let pmh7_base = h8.resources.pmh7_base.raw();
    let mut h8 = *h8;
    h8.has_bluetooth = super::bluetooth_present();
    out.extend(crate::h8_acpi::dsdt_aml_with_brightness(
        &h8,
        lpc_scope,
        &brightness,
    ));
    // These are EC-hosted PS/2 devices, not nonexistent NSC KBC LDNs.
    let mut lpc = fstart_acpi::devices::i8042_aml(0x60, 0x64, 1, Some(12));
    lpc.extend(
        fstart_acpi::devices::GenericAcpi {
            name: "DRES",
            hid: "PNP0C02",
            resources: &super::dock::FIRMWARE_RESOURCES,
            gsiv: None,
        }
        .dsdt_aml(),
    );
    out.extend(fstart_acpi::aml_linker::scope_vec(lpc_scope, &lpc).expect("T6 LPC resources"));
    out.extend(Vec::<u8>::from(acpi_dsl! {
        Scope(#{const "\\_SB_.PCI0.LPCB.EC__"}) {
            OperationRegion("PMIO", SystemIO, #{word pmh7_base}, 0x10u32);
            Field("PMIO", ByteAcc, NoLock, Preserve) {
                Offset(0x0C), PIDX, 8, PIDH, 8, DPWR, 1,
            }
            Method("_Q18", 0, Serialized) { Notify(#{const "\\_SB_.DOCK"}, 3u32); }
            Method("_Q37", 0, Serialized) {
                PIDH = 0u32;
                PIDX = 0x61u32;
                If (DPWR) { Notify(#{const "\\_SB_.DOCK"}, 0u32); }
                Else { Notify(#{const "\\_SB_.DOCK"}, 3u32); }
            }
            Method("_Q50", 0, Serialized) {
                If (#{const "\\_SB_.DOCK._STA"}()) { Notify(#{const "\\_SB_.DOCK"}, 1u32); }
            }
        }
    }));
    out
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::{fs, process::Command};

    #[test]
    fn t6_dock_namespace_round_trips_with_and_without_igd_hooks() {
        for native in [false, true] {
            let mut aml = acpi_dsl! {
                Scope(#{const "\\_SB_"}) {
                    Device("PCI0") {
                        Device("LPCB") {}
                        Device("GFX0") {
                            Method("INCB", 0, NotSerialized) {}
                            Method("DECB", 0, NotSerialized) {}
                        }
                    }
                }
            }
            .to_vec();
            aml.extend(dsdt_aml(
                &super::super::h8_config(0x1c),
                "\\_SB_.PCI0.LPCB",
                native,
            ));
            // Every declared firmware range is both consumed in AML and
            // supplied by the board's PCI-allocation hook.
            let config = super::super::h8_config(0x1c);
            for resource in super::super::firmware_resources(&config) {
                if let fstart_core::acpi::AcpiResource::Pio { base, size } = resource {
                    let [lo, hi] = base.to_le_bytes();
                    assert!(
                        aml.windows(8)
                            .any(|bytes| bytes[..6] == [0x47, 1, lo, hi, lo, hi]
                                && bytes[7] == size as u8)
                    );
                }
            }
            for irq in [[0x23, 2, 0, 1], [0x23, 0, 0x10, 1]] {
                assert!(aml.windows(irq.len()).any(|bytes| bytes == irq));
            }
            let dir = std::env::temp_dir().join(std::format!(
                "fstart-t6-aml-{}-{native}",
                std::process::id()
            ));
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join("dsdt.aml"),
                fstart_acpi::platform::build_dsdt(&aml),
            )
            .unwrap();
            for args in [&["-d", "dsdt.aml"][..], &["-oa", "-tc", "dsdt.dsl"][..]] {
                let output = Command::new("iasl")
                    .current_dir(&dir)
                    .args(args)
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "iasl: {}{}",
                    std::string::String::from_utf8_lossy(&output.stdout),
                    std::string::String::from_utf8_lossy(&output.stderr)
                );
            }
            let source = fs::read_to_string(dir.join("dsdt.dsl")).unwrap();
            for name in [
                "Device (DOCK)",
                "Device (DRES)",
                "Device (PS2K)",
                "Device (PS2M)",
                "Method (_Q18",
                "Method (_Q37",
                "Method (_Q50",
            ] {
                assert_eq!(source.matches(name).count(), 1, "{name}");
            }
            assert_eq!(source.contains("GFX0.INCB ()"), native);
            assert!(source.contains("Method (SMIC, 1, Serialized)"));
            assert!(source.contains("Return (SMIC (0x10))"));
            assert!(!source.contains("DSTA"));
            fs::remove_dir_all(dir).unwrap();
        }
    }
}
