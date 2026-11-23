use super::{ECAM, PCI_ROOT, iasl_round_trip};
use fstart_acpi::{
    Aml,
    aml::{Arg, If, Method, MethodCall, Path, Return},
    aml_linker::scope_vec,
    device::AcpiDevice,
    devices::i2c::I2cDevice,
};
use fstart_acpi_macros::acpi_dsl;
use fstart_driver_intel::{
    IntelNorthbridgeDriver, IntelSouthbridgeDriver,
    haswell::*,
    lynxpoint::{gpio::*, *},
    namespace::SouthbridgeAcpi,
    southbridge::{gpio_ich::GpioConfig, hda::HdaConfig},
};
use fstart_pci::ecam::{self, EcamDevice};

/// Board-like fragments exercising real Haswell + Lynx Point ACPI paths
/// (graphics backlight, USB port descriptors, Serial IO I2C child wake,
/// and MCHC cTDP throttling), without External stubs.
fn lp_board_children() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&acpi_dsl! {
        Scope("\\_SB_.PCI0.GFX0") {
            Method("BTST", 0, Serialized) {
                // First access to GFRG resolves the live config BAR, not the
                // address the synthetic host bridge used to emit this DSDT.
                BAR0 = 0xd2000004u32;
                ASLS = 0u32;
                BCLM = 100u32;
                XBCM(50u32);
                DECB();
                If (BCLV != 49u32) { Return(1u32); }
                Local0 = #{const "\\_SB_.PCI0.GFX0.LCD0._BCL"};
                If (SizeOf(Local0) != 103u32) { Return(2u32); }
                INCB();
                If (BCLV != 49u32) { Return(3u32); }
                // The driver restored a different duty after the OS request.
                BCLV = 20u32;
                If (XBQC() != 49u32) { Return(4u32); }
                If (BCLV != 49u32) { Return(5u32); }
                Return(0u32);
            }
        }
        Scope("\\_SB_.PCI0.XHCI.HUB7.PRT2") {
            Name("_UPC", Package(0xffu8, 0u8, 0u8, 0u8));
            Method("_PLD", 0, NotSerialized) { Return(GPLD(1u8)); }
        }
        Scope("\\_SB_.PCI0.XHCI.HUB7.SSP1") {
            Name("_UPC", Package(0xffu8, 3u8, 0u8, 0u8));
        }
    });
    let bus = "\\_SB_.PCI0.I2C0";
    let child = I2cDevice {
        name: "ETPA",
        ids: &["ELAN0000"],
        uid: 1,
        bus,
        address: 0x15,
        speed_hz: 400_000,
        interrupt: None,
        hid_descriptor: None,
    };
    out.extend(scope_vec(bus, &child.dsdt_aml().unwrap()).unwrap());
    let wake = MethodCall::new(Path::new("\\_SB_.PCI0.LPCB.GPIO.GWAK"), vec![&33u8]);
    let mut methods = Vec::new();
    Method::new(
        "_DSW".into(),
        3,
        false,
        vec![&If::new(&Arg(0), vec![&wake])],
    )
    .to_aml_bytes(&mut methods);
    out.extend(scope_vec("\\_SB_.PCI0.I2C0.ETPA", &methods).unwrap());
    let enable = MethodCall::new(Path::new("\\_SB_.PCI0.MCHC.CTLE"), vec![&7u8]);
    let disable = MethodCall::new(Path::new("\\_SB_.PCI0.MCHC.CTLD"), vec![]);
    let mut zone = Vec::new();
    Method::new(
        "THRT".into(),
        1,
        true,
        vec![
            &If::new(&Arg(0), vec![&Return::new(&enable)]),
            &Return::new(&disable),
        ],
    )
    .to_aml_bytes(&mut zone);
    zone.extend_from_slice(&acpi_dsl! {
        ThermalZone("THRM") {
            Name("_TC1", 2u8);
            Name("_TC2", 5u8);
            Name("_TSP", 20u8);
            Method("_PSV", 0, NotSerialized) { Return(3450u32); }
            Method("_CRT", 0, NotSerialized) { Return(3650u32); }
            Method("_PSL", 0, NotSerialized) { Return(PPKG()); }
        }
    });
    out.extend(scope_vec("\\_TZ_", &zone).unwrap());
    out
}

/// Exercise the AML interpreter, not just the spelling of names in ASL.
fn evaluate(chipset: &str, commands: &str) -> Vec<u64> {
    let dir = std::env::temp_dir().join(format!("fstart-{chipset}-dsdt-{}", std::process::id()));
    let output = std::process::Command::new("acpiexec")
        .current_dir(dir)
        .args(["-dt", "-fv", "0", "-b", commands, "roundtrip.aml"])
        .output()
        .unwrap();
    let log = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success() && !log.contains("AE_") && !log.contains("ACPI Error"),
        "{log}"
    );
    log.lines()
        .filter_map(|line| line.trim().strip_prefix("[Integer] = "))
        .map(|hex| u64::from_str_radix(hex, 16).unwrap())
        .collect()
}

#[test]
fn populated_lp() {
    let _guard = ECAM.lock().unwrap();
    static PADS: PadTable = [Pad::new(Mode::Native); 95];
    static NB: HaswellConfig = HaswellConfig::new(true);
    static SB: LynxPointConfig = LynxPointConfig {
        usb2: &[Usb2Port {
            length: 0x80,
            enabled: true,
            oc_pin: Some(0),
            location: Location::BackPanel,
        }; 8],
        usb3: &[Usb3Port {
            enabled: true,
            oc_pin: Some(0),
        }; 4],
        serialio_enable: 7,
        sio_acpi_mode: true,
        hda: Some(&HdaConfig::new()),
        ..LynxPointConfig::new(Gpios::Lp(&PADS))
    };
    let nb = Haswell::new_from_config(&NB).unwrap();
    let mut sb = LynxPoint::new_from_config(&SB).unwrap();
    // Aligned ECAM window for bus 0. Restore the old binding before freeing it.
    let mut bus = vec![0xffu8; 2 << 20];
    struct Reset(usize);
    impl Drop for Reset {
        fn drop(&mut self) {
            ecam::init(self.0);
        }
    }
    let _reset = Reset(ecam::base());
    ecam::init(bus.as_mut_ptr().addr().next_multiple_of(1 << 20));
    let gfx = EcamDevice::new(0, 2, 0);
    gfx.write16(0, 0x8086);
    gfx.write32(0x10, 0xd000_0000);
    gfx.write8(0x3d, 1);
    for i in 0..3 {
        let d = EcamDevice::new(0, 21, i);
        d.write16(0, 0x8086);
        d.write8(0x3d, 0);
        d.write32(0x10, 0xe000_0000 + 0x2000 * u32::from(i));
        d.write32(0x14, 0xe000_1000 + 0x2000 * u32::from(i));
    }
    let cold = sb.acpi_bar_snapshot().unwrap();
    assert_eq!(
        &cold[0..9],
        &[SB.pcie_ports, 255, 255, 255, 255, 255, 255, 255, 255]
    );
    assert_eq!(cold.len(), 9 + 3 * 9);
    let mut aml = nb.dsdt_aml(&NB);
    aml.extend(sb.dsdt_aml(&SB, &PCI_ROOT));
    aml.extend(lp_board_children());
    let dsl = iasl_round_trip("haswell-lp", &aml);
    assert!(!dsl.contains("External ("), "dangling reference: {dsl}");
    for required in [
        "Method (PNOT",
        "Method (PPCN",
        "Method (TNOT",
        "BAR0 & 0xFFFFFFFFFFFFFFF0",
        "Method (GWAK",
        "Method (CTLE",
        "Method (CTLD",
        "Method (PPKG",
        "ThermalZone (THRM)",
        "Device (I2C1)",
        "Device (PRT8)",
        "Device (SSP4)",
        "Device (ETPA)",
        "0xE0002000",
        "0xE0003000",
        "0x0002FFFF",
        "Device (LCD0)",
    ] {
        assert!(dsl.contains(required), "missing {required}");
    }
    assert!(!dsl.contains("Device (EHC1)"));
    assert!(!dsl.contains("Device (PEGP)"));
    let states = evaluate(
        "haswell-lp",
        concat!(
            r"execute \PNOT;execute \PPCN;execute \TNOT;execute \_SB.PCI0.GFX0.BTST;execute \_SB.PCI0.XHCI.HUB7.PRT2._PLD;execute \_TZ.THRM._PSL;execute \_SB.PCI0.I2C0.ETPA._DSW 1 3 0;",
            r"execute \_TZ.THRT 1;execute \_SB.C000._PPC;execute \_SB.PCI0.MCHC.PL1V;",
            r"execute \_TZ.THRT 0;execute \_SB.C000._PPC;",
            r"execute \_SB.PCI0.XHCI._PRW;execute \_SB.PCI0.HDEF._PRW"
        ),
    );
    // BTST=0, THRT(1)=1, _PPC=4, PL1V=7, THRT(0)=1, _PPC=0, XHCI/HDEF _PRW=(0x6d, 3).
    assert_eq!(states, [0, 1, 4, 7, 1, 0, 0x6d, 3, 0x6d, 3]);
    EcamDevice::new(0, 21, 1).write32(0x14, 0xe001_0000);
    assert_ne!(cold, sb.acpi_bar_snapshot().unwrap());
}

#[test]
fn traditional_haswell() {
    let _guard = ECAM.lock().unwrap();
    static NB: HaswellConfig = HaswellConfig {
        minihd: false,
        ..HaswellConfig::new(false)
    };
    static SB: LynxPointConfig = LynxPointConfig {
        xhci_default: false,
        hda: Some(&HdaConfig::new()),
        ..LynxPointConfig::new(Gpios::Traditional(&GpioConfig::new()))
    };
    let nb = Haswell::new_from_config(&NB).unwrap();
    let sb = LynxPoint::new_from_config(&SB).unwrap();
    let mut aml = nb.dsdt_aml(&NB);
    aml.extend(sb.dsdt_aml(&SB, &PCI_ROOT));
    let dsl = iasl_round_trip("haswell-trad", &aml);
    for node in ["PEGP", "PEG1", "PEG2", "EHC1", "EHC2", "HDEF", "SSP6"] {
        assert!(dsl.contains(&format!("Device ({node})")));
    }
    assert_eq!(
        evaluate(
            "haswell-trad",
            r"execute \_SB.PCI0.HDEF._PRW;execute \_SB.PCI0.EHC1._PRW;execute \_SB.PCI0.EHC2._PRW;execute \_SB.PCI0.XHCI._PRW"
        ),
        [13, 3, 13, 3, 13, 3, 13, 3]
    );
    for absent in ["HDAU", "INT33C7", "Device (SDMA)"] {
        assert!(!dsl.contains(absent));
    }
}
