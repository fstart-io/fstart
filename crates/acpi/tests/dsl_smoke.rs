use fstart_acpi_macros::acpi_dsl;

#[test]
fn tz_incremental() {
    let hp = |s: &str| fstart_acpi::aml::Path::new(s);
    let tzp = 100u32;
    let tc1 = 0x02u32;
    let tc2 = 0x05u32;
    let base: Vec<u8> = acpi_dsl! {
        Scope("\\_TZ_") {
            Method("CTOK", 1, NotSerialized) {
                Local0 = Arg0 * 10u32;
                Local0 += 2732u32;
                If (Local0 <= 2732u32) { Return(3000u32); }
                If (Local0 > 4012u32) { Return(3000u32); }
                Return(Local0);
            }
            ThermalZone("THM0") {
                Name("_TZP", #{tzp});
                Name("_TSP", #{tzp});
                Name("_TC1", #{tc1});
                Name("_TC2", #{tc2});
            }
        }
    };
    assert!(base.len() > 10);

    // + _PSL/GCRT/GPSV
    let more: Vec<u8> = acpi_dsl! {
        ThermalZone("THM0") {
            Method("_PSL", 0, Serialized) { Return(PPKG()); }
            Method("GCRT", 0, NotSerialized) {
                Local0 = TCRT;
                If (Local0 > 0u32) { Return(Local0); }
                Return(127u32);
            }
            Method("GPSV", 0, NotSerialized) {
                Local0 = TPSV;
                If (Local0 > 0u32) { Return(Local0); }
                Return(95u32);
            }
        }
    };
    assert!(more.len() > 10);

    // + _CRT/_PSV with CTOK calls
    let calls: Vec<u8> = acpi_dsl! {
        ThermalZone("THM0") {
            Method("_CRT", 0, NotSerialized) { Return(CTOK(GCRT())); }
            Method("_PSV", 0, NotSerialized) { Return(CTOK(GPSV())); }
        }
    };
    assert!(calls.len() > 10);

    // + _TMP/_AC0
    let tmpac: Vec<u8> = acpi_dsl! {
        ThermalZone("THM0") {
            Method("_TMP", 0, NotSerialized) {
                Local0 = TMP0;
                If (Local0 == 128u32) { Return(CTOK(40u32)); }
                Return(CTOK(Local0));
            }
            Method("_AC0", 0, NotSerialized) {
                Local0 = GPSV();
                Local0 -= 10u32;
                If (FLVL != 0u32) { Local0 -= 5u32; }
                Return(CTOK(Local0));
            }
        }
    };
    assert!(tmpac.len() > 10);

    // + _AL0/PwrRes/FAN
    let fan: Vec<u8> = acpi_dsl! {
        Name("_AL0", Package(#{hp("FAN_")}));
        PowerResource("FPWR", 0u8, 0u16) {
            Method("_STA", 0, NotSerialized) { Return(FLVL); }
            Method("_ON", 0, NotSerialized) {
                FANE(1u32);
                FLVL = 1u32;
                Notify(#{hp("\\_TZ_.THM0")}, 0x82u32);
            }
            Method("_OFF", 0, NotSerialized) {
                FANE(0u32);
                FLVL = 0u32;
                Notify(#{hp("\\_TZ_.THM0")}, 0x82u32);
            }
        }
        Device("FAN_") {
            Name("_HID", EisaId("PNP0C0B"));
            Name("_PR0", Package(#{hp("FPWR")}));
        }
    };
    assert!(fan.len() > 10);
}

#[test]
fn charge_threshold_fragments_disassemble_clean() {
    // These mirror hkey_charge_behaviour_aml / hkey_thresholds_aml; the real
    // validation is iasl on the dumped table (see driver-lenovo tests).
    let aml: Vec<u8> = acpi_dsl! {
        Scope("\\_SB_.PCI0.LPCB.EC__") {
            Field("ERAM", ByteAcc, NoLock, Preserve) {
                Offset(0x0F),
                B0IC, 1,
                B1IC, 1,
            }
            Method("RBCB", 1, NotSerialized) {
                If (Arg0 == 0u32) {
                    BDSS(0x100u32);
                    BDSS(0x200u32);
                }
            }
        }
    };
    assert!(aml.len() > 10);
}
