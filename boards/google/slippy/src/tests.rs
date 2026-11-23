// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use fstart_driver_intel::gma::{OutputConfig, Port, panel::PanelPowerDelays};
use fstart_platform_intel::haswell::{Haswell, Mode, Pull, Slot};

#[test]
fn peppy_gma_and_vbt_match_slippy_hardware() {
    use fstart_driver_intel::IntelNorthbridgeDriver;
    let igd = PLATFORM.northbridge.igd;
    assert_eq!(igd.panel.hotplug, [6, 0, 6]);
    assert_eq!(
        igd.panel.delays,
        Some(PanelPowerDelays {
            power_up_us: 40_000,
            power_down_us: 15_000,
            power_cycle_us: 400_000,
            power_up_to_backlight_on_us: 210_000,
            backlight_off_to_power_down_us: 210_000,
        })
    );
    assert_eq!(igd.panel.backlight_pwm_hz, 200);
    assert!(!igd.panel.backlight_active_low);
    assert!(!igd.panel.ddi_e_connected);
    assert!(igd.brightness_levels.is_empty());
    assert_eq!(igd.vbt.and_then(|v| v.file), Some("data.vbt"));

    let display = igd
        .display
        .expect("Peppy native Haswell GMA display policy");
    assert_eq!(
        display.outputs,
        &[
            OutputConfig {
                port: Port::Edp,
                enabled: true,
            },
            OutputConfig {
                port: Port::HdmiA,
                enabled: true,
            },
        ]
    );
    let fallback = display
        .framebuffer
        .fallback_mode
        .expect("Peppy fallback mode");
    assert_eq!(
        (fallback.width, fallback.height, fallback.refresh_hz),
        (1366, 768, 60)
    );

    // Embedded data.vbt starts with the "$VBT" header and passes length check.
    let vbt = include_bytes!("../data.vbt");
    assert_eq!(&vbt[..4], b"$VBT");
    let size = u16::from_le_bytes([vbt[24], vbt[25]]) as usize;
    assert!(size >= 26 && size <= vbt.len());

    // Haswell driver accepts the full Peppy northbridge configuration.
    assert!(Haswell::new_from_config(&PLATFORM.northbridge).is_ok());
}

#[test]
fn every_ram_id_and_ec_revision() {
    let numbers: [&[u8]; 4] = [
        b"4KTF25664HZ-1G6E1 ",
        b"HMT425S6AFR6A-PB  ",
        b"EDJ4216EFBG-GN-F\0\0",
        b"HMT425S6CFR6A-PB  ",
    ];
    for version in 0..=u16::MAX {
        for id in 0..8 {
            let [[Slot::MemoryDown(spd), Slot::Empty], [other, Slot::Empty]] =
                spd::population(id, version)
            else {
                panic!("slot wiring")
            };
            assert_eq!(&spd[128..146], numbers[(id % 4) as usize]);
            let ch1 = match other {
                Slot::Empty => false,
                Slot::MemoryDown(b) => {
                    assert!(core::ptr::eq(spd, b));
                    true
                }
                _ => panic!("not a socket"),
            };
            let populated = if version == 0 {
                0b1111_1110u8
            } else {
                0b0000_1111
            };
            assert_eq!(ch1, populated & (1 << id) != 0);
        }
    }
}

#[test]
fn physical_gpio_map_matches_peppy_gpio_c() {
    for (pin, pad) in pads::PADS.iter().enumerate() {
        use Mode::*;
        let mode = match pin {
            4..=7 | 18 | 19 | 30..=33 | 40 | 42 | 60 | 63 | 71..=75 | 81 | 82 => Native,
            8 | 10 | 36 => Sci,
            34 | 35 => Smi,
            9 | 12..=14 | 25 | 47 | 52 | 58 => Input,
            44..=46 | 57 | 59 => Output(true),
            51 | 53 | 54 => Pirq,
            _ => Unused,
        };
        assert_eq!(pad.mode, mode, "GPIO{pin}");
        assert_eq!(pad.invert, matches!(pin, 12 | 25));
        assert!(!pad.blink && !pad.rsmrst && matches!(pad.pull, Pull::None));
    }
}

#[test]
fn alc283_matches_cim_and_beep_tables() {
    let codec = hda::ALC283.verbs.get(0);
    assert_eq!(
        (codec.vendor_id, codec.subsystem_id),
        (0x10ec0283, 0x11790670)
    );
    assert_eq!(codec.pre_verbs, [0x0017ff00, 0x0017ff00, 0xf0000, 0xf0002]);
    let pins = codec
        .pins
        .as_slice()
        .iter()
        .map(|p| (p.nid, p.encode()))
        .collect::<std::vec::Vec<_>>();
    const NC: u32 = 0x411111f0;
    assert_eq!(
        pins,
        [
            (0x12, NC),
            (0x14, 0x90170110),
            (0x17, NC),
            (0x18, NC),
            (0x19, 0x03a11020),
            (0x1a, 0x90a70111),
            (0x1b, NC),
            (0x1d, 0x4015812d),
            (0x1e, NC),
            (0x21, 0x03211021)
        ]
    );
    assert_eq!(
        codec.extra_verbs.as_slice(),
        [
            0x02050010, 0x02040c20, 0x0205001b, 0x0204081b, 0x02050043, 0x0204a614, 0x02050047,
            0x02049470, 0x00170500, 0x01470740, 0x01470c02, 0x0143b01f, 0x00c37100, 0x00b37410
        ]
    );
}

#[cfg(feature = "acpi")]
#[test]
fn absent_i2c_controller_has_no_children_and_tpm2_has_a_table() {
    use fstart_acpi::device::AcpiDevice;
    use fstart_platform_intel::haswell::SerialIoMask;
    assert!(PLATFORM.southbridge.sealed_case);
    assert_eq!(
        PLATFORM.southbridge.serialio_enable,
        SerialIoMask::SDMA | SerialIoMask::I2C0 | SerialIoMask::I2C1
    );
    for mask in [
        0,
        SerialIoMask::I2C0,
        SerialIoMask::I2C1,
        SerialIoMask::I2C0 | SerialIoMask::I2C1,
    ] {
        let board = acpi::Acpi {
            serial_io: SerialIoMask(mask),
            devices: options::Devices {
                ambient_light: true,
                touchpad_type: 0,
                lte_ngff: false,
            },
            tpm: Some(fstart_driver_tpm::Family::Tpm20),
            tpm_irq: None,
            wlan: true,
            ec_routed: false,
        };
        let aml = board.dsdt_aml(&());
        for (text, want) in [
            (b"ATML0001", mask & SerialIoMask::I2C1 != 0),
            (b"CYAP0000", mask & SerialIoMask::I2C0 != 0),
            (b"ELAN0000", mask & SerialIoMask::I2C0 != 0),
            (b"MSFT0101", true),
        ] {
            assert_eq!(aml.windows(8).any(|w| w == text), want);
        }
        assert!(aml.windows(4).any(|w| w == b"WLAN"));
        assert_eq!(&board.extra_tables(&())[0][..4], b"TPM2");
    }
}

#[cfg(feature = "acpi")]
#[test]
fn board_dsdt_round_trip() {
    use fstart_acpi::device::AcpiDevice;
    use fstart_driver_intel::{
        IntelNorthbridgeDriver, IntelSouthbridgeDriver,
        namespace::{SouthbridgeAcpi, pci_root},
    };
    use fstart_pci::ecam::{self, EcamDevice};
    use fstart_platform_intel::haswell::LynxPoint;
    use std::{fs, process::Command};
    // One emulated ECAM bus; no host I/O. BAR capture is the same step as
    // ramstage before hiding Serial IO. Restore the global before freeing it.
    let mut backing = std::vec![0xffu8;2<<20];
    struct Reset(usize);
    impl Drop for Reset {
        fn drop(&mut self) {
            ecam::init(self.0);
        }
    }
    let _reset = Reset(ecam::base());
    ecam::init(backing.as_mut_ptr().addr().next_multiple_of(1 << 20));
    for i in 0..3 {
        let d = EcamDevice::new(0, 21, i);
        d.write16(0, 0x8086);
        d.write8(0x3d, 0);
        d.write32(0x10, 0xe0000000 + u32::from(i) * 0x2000);
        d.write32(0x14, 0xe0001000 + u32::from(i) * 0x2000);
    }
    let nb = Haswell::new_from_config(&PLATFORM.northbridge).unwrap();
    let mut sb = LynxPoint::new_from_config(&PLATFORM.southbridge).unwrap();
    sb.acpi_bar_snapshot().unwrap();
    let mut chipset = nb.dsdt_aml(&PLATFORM.northbridge);
    chipset.extend(sb.dsdt_aml(&PLATFORM.southbridge, &pci_root::<Haswell>()));
    // Test-only view of GPIOBASE. Seed unrelated bits as well so _DSW must
    // update the correct pin without replacing its neighbouring pads/bank.
    chipset.extend_from_slice(&fstart_acpi_macros::acpi_dsl! { Scope("\\") {
        OperationRegion("TGIO", SystemIO, 0x1400u32, 0x200u32);
        Field("TGIO", DWordAcc, NoLock, Preserve) {
            TOWN,32, Offset(0x30), TRTE,32, Offset(0x160), TP12,32, Offset(0x1c8), TP25,32,
        }
        Method("TINI",0,Serialized){ TOWN=0xffffffffu32; TRTE=0xffffffffu32; TP12=0x10u32; TP25=0x10u32; }
        Method("TGET",0,Serialized){ Return(Package(TOWN,TRTE,TP12,TP25)); }
        Method("TSET",2,Serialized){Store(Arg0,#{const "\\_SB_.PCI0.LPCB.EC0_.ACEX"});Store(Arg1,#{const "\\_SB_.PCI0.LPCB.EC0_.LIDS"});}
    }});
    let dir = std::env::temp_dir().join(std::format!("fstart-peppy-dsdt-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    for (tp, optional) in [(0, false), (1, true), (2, true)] {
        let mut aml = chipset.clone();
        aml.extend(
            acpi::Acpi {
                devices: options::Devices {
                    touchpad_type: tp,
                    ambient_light: optional,
                    lte_ngff: optional,
                },
                tpm: Some(fstart_driver_tpm::Family::Tpm12),
                tpm_irq: optional.then_some(fstart_driver_tpm::tis::Irq {
                    vector: 5,
                    low: true,
                    edge: false,
                }),
                serial_io: sb.serial_io_populated(),
                ec_routed: true,
                wlan: false, // fixture doesn't enumerate a PCIe endpoint
            }
            .dsdt_aml(&()),
        );
        let sim_pld = [
            0x82, 0, 0, 0, 0, 0, 0, 0, 0x71, 0x1d, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        assert_eq!(aml.windows(sim_pld.len()).any(|w| w == sim_pld), optional);
        fs::write(
            dir.join("dsdt.aml"),
            fstart_acpi::platform::build_dsdt(&aml),
        )
        .unwrap();
        for args in [
            &["-d", "dsdt.aml"][..],
            &["-oa", "-p", "roundtrip", "dsdt.dsl"],
        ] {
            let o = Command::new("iasl")
                .current_dir(&dir)
                .args(args)
                .output()
                .unwrap();
            assert!(
                o.status.success(),
                "{}{}",
                std::string::String::from_utf8_lossy(&o.stdout),
                std::string::String::from_utf8_lossy(&o.stderr)
            );
        }
        let dsl = fs::read_to_string(dir.join("dsdt.dsl")).unwrap();
        assert!(!dsl.contains("External ("), "unresolved board path");
        for name in [
            "ATML0001",
            "atmel,maxtouch",
            "GOOG0004",
            "GOOG000A",
            "PNP0C31",
            "ThermalZone (THRM)",
            "Method (_Q12",
            "CTLE (0x0C)",
            "GPIO.GWAK (",
            "Name (_GPE, 0x24)",
        ] {
            assert!(dsl.contains(name), "missing {name}");
        }
        let tpm = dsl
            .split_once("Device (TPM)")
            .unwrap()
            .1
            .split("Device (")
            .next()
            .unwrap();
        for value in ["0x89E37D9C", "0x0C31", "_CID"] {
            assert!(tpm.contains(value), "{tpm}");
        }
        assert_eq!(
            tpm.contains("Interrupt (ResourceConsumer, Level, ActiveLow, Shared"),
            optional,
            "{tpm}"
        );
        assert!(dsl.contains("Name (_S4,") && !dsl.contains("Name (_S1,"));
        assert!(!dsl.contains("GOOG0007"));
        if tp == 0 {
            let commands = concat!(
                r"execute \TSET 1 1;execute \_WAK 3;execute \_WAK 3;execute \TSET 0 1;execute \_SB.PCI0.LPCB.EC0._Q05;execute \PWRS;execute \_SB.PCI0.LPCB.EC0._Q17;",
                r"execute \_SB.PCI0.LPCB.EC0.TSRD 0xff;execute \_SB.PCI0.LPCB.EC0.PAT1 5 0xce4;execute \_SB.PCI0.LPCB.EC0.PATT;execute \_SB.PCI0.LPCB.EC0.PATC;execute \_SB.PCI0.LPCB.EC0._Q09;execute \_SB.PCI0.LPCB.EC0.PATD 5;execute \_SB.PCI0.LPCB.EC0.CHGD;execute \_SB.PCI0.LPCB.EC0.CHGL"
            );
            let out = Command::new("acpiexec")
                .current_dir(&dir)
                .args(["-dt", "-fv", "0", "-b", commands, "roundtrip.aml"])
                .output()
                .unwrap();
            let log = std::string::String::from_utf8_lossy(&out.stdout);
            assert!(out.status.success() && !log.contains("AE_"), "{log}");
            let values = log
                .lines()
                .filter_map(|l| l.trim().strip_prefix("[Integer] = "))
                .map(|v| u64::from_str_radix(v, 16).unwrap())
                .collect::<std::vec::Vec<_>>();
            assert!(values.ends_with(&[2000, 1, 130, 3, 1, 255]), "{log}");
            // One AC and lid notification on the first wake, none on the
            // second. The disconnect raises the second AC notification.
            assert_eq!(log.matches("Notify on [AC__]").count(), 2, "{log}");
            assert_eq!(log.matches("Notify on [LID0]").count(), 1, "{log}");
            assert!(
                log.contains("Notify on [BAT0]") && log.contains("Notify on [C000]"),
                "{log}"
            );
        }
        assert!(dsl.contains(r"Notify (\_SB.CREC, 0x02)"));
        assert!(dsl.contains("Method (_REG") && dsl.contains("PNOT ()"));
        assert_eq!(dsl.contains("ISL29018"), optional);
        if optional {
            let als = dsl
                .split_once("Device (ALSI)")
                .unwrap()
                .1
                .split("Device (")
                .next()
                .unwrap();
            assert!(
                als.contains("0x00000023"),
                "ALSI must advertise PIRQT/IRQ35: {als}"
            );
            assert_eq!(pads::PADS[51].mode, Mode::Pirq);
        }
        assert_eq!(dsl.contains("ELAN0000"), tp != 2);
        assert_eq!(dsl.contains("CYAP0000"), tp != 1);
        assert_eq!(
            dsl.matches("Name (_UPC,").count(),
            if optional { 8 } else { 7 }
        );
        // Execute this fixture now, before the next one replaces roundtrip.aml.
        let pads: &[&str] = match tp {
            1 => &["ETPA"],
            2 => &["CTPA"],
            _ => &["ETPA", "CTPA"],
        };
        for pad in pads {
            for (fill, temperature) in [("0", 2000), ("0xff", 2732)] {
                let commands = std::format!(
                    concat!(
                        r"execute \_TZ.THRM._TMP;execute \_TZ.THRM._PSV;execute \_TZ.THRM._CRT;execute \TINI;",
                        r"execute \_SB.PCI0.I2C0.{0}._DSW 1 3 0;execute \TGET;execute \_SB.PCI0.I2C0.{0}._PRW;",
                        r"execute \_SB.PCI0.I2C1.ATSA._DSW 1 3 0;execute \TGET;execute \_SB.PCI0.I2C1.ATSA._PRW;",
                        r"execute \_SB.PCI0.LPCB.EC0.LID0._PRW;execute \_SB.PCI0.LPCB.EC0._Q12;execute \_SB.PCI0.MCHC.PL1V"
                    ),
                    pad
                );
                let output = Command::new("acpiexec")
                    .current_dir(&dir)
                    .args(["-dt", "-fv", fill, "-b", &commands, "roundtrip.aml"])
                    .output()
                    .unwrap();
                let log = std::format!(
                    "{}{}",
                    std::string::String::from_utf8_lossy(&output.stdout),
                    std::string::String::from_utf8_lossy(&output.stderr)
                );
                assert!(
                    output.status.success() && !log.contains("AE_") && !log.contains("ACPI Error"),
                    "{log}"
                );
                let values = log
                    .lines()
                    .filter_map(|s| s.trim().strip_prefix("[Integer] = "))
                    .map(|s| u64::from_str_radix(s, 16).unwrap())
                    .collect::<std::vec::Vec<_>>();
                let gpio12 = u64::from(u32::MAX ^ (1 << 12));
                let both = gpio12 ^ (1 << 25);
                assert_eq!(values.len(), 18, "{log}");
                assert_eq!(
                    &values[..17],
                    [
                        temperature,
                        3682,
                        3722,
                        gpio12,
                        gpio12,
                        0x1d,
                        0x10,
                        12,
                        3,
                        both,
                        both,
                        0x1d,
                        0x1d,
                        25,
                        3,
                        0x69,
                        5
                    ],
                    "{log}"
                );
                if fill == "0" {
                    assert_eq!(values[17], 12, "12 W at simulated PUNI=0");
                }
            }
        }
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn smbios_onboard_matches_coreboot_slippy() {
    fn summary<'a>(
        v: &[fstart_platform_intel::tables::ExtraOnboardDevice<'a>],
    ) -> std::vec::Vec<(&'a str, u8, u16, u8, u8)> {
        v.iter()
            .map(|e| (e.device.designation, e.instance, e.segment, e.bus, e.devfn))
            .collect()
    }
    let base = crate::acpi::smbios_onboard(false);
    assert_eq!(
        summary(&base).as_slice(),
        &[
            ("trackpad", 37, 1, 0x67, 0),
            ("touchscreen", 38, 2, 0x4a, 0)
        ]
    );

    let with_als = crate::acpi::smbios_onboard(true);
    assert_eq!(
        summary(&with_als).as_slice(),
        &[
            ("lightsensor", 35, 2, 0x44, 0),
            ("trackpad", 37, 1, 0x67, 0),
            ("touchscreen", 38, 2, 0x4a, 0),
        ]
    );
}
