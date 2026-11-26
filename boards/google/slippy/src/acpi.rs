// SPDX-License-Identifier: MIT OR Apache-2.0
//! Board additions to the Haswell / Lynx Point DSDT. No GNVS or External
//! stubs: device options are resolved before the table is built.
extern crate alloc;
use crate::options::Devices;
use alloc::{vec, vec::Vec};
use fstart_acpi::{
    Aml, PcNamespace,
    aml::{Interrupt, Name, ResourceTemplate},
    aml_linker::{device_vec, scope_vec},
    descriptors::i2c::I2cSerialBus,
    device::AcpiDevice,
    devices::tpm::{TpmFamily, TpmIrq, TpmTisAcpi, TpmTisOptions},
    smbios::SmbiosIdentity,
};
use fstart_acpi_macros::acpi_dsl;
use fstart_driver_chromeec::acpi::{Ec, LpcPolicy, PowerNotify, ThermalPolicy};
use fstart_driver_tpm::Family;
use fstart_platform_intel::haswell::{HaswellUlt, SerialIoMask};

pub const IDENTITY: SmbiosIdentity<'static> = SmbiosIdentity {
    bios_vendor: "fstart",
    bios_version: "fstart Peppy",
    bios_release_date: fstart_platform_intel::SMBIOS_RELEASE_DATE,
    sys_manufacturer: "GOOGLE",
    sys_product: "Peppy",
    sys_version: "1.0",
    sys_serial: None,
    sys_uuid: None,
    bb_manufacturer: "GOOGLE",
    bb_product: "Peppy",
    bb_version: "1.0",
    bb_serial: None,
    chassis_type: 0x0a,
    chassis_manufacturer: "Acer",
    chassis_serial: None,
    processor_sockets: &["BGA1168"],
    oem_string: None,
    ipmi: None,
};
/// Peppy SMBIOS Type 41 onboard I2C device records consumed by Linux
/// `chromeos_laptop` (coreboot `google/slippy/mainboard.c:mainboard_smbios_data`).
#[must_use]
pub fn smbios_onboard(
    ambient_light: bool,
) -> Vec<fstart_platform_intel::tables::ExtraOnboardDevice<'static>> {
    use fstart_core::smbios::{OnboardDevice, OnboardType};
    use fstart_platform_intel::tables::ExtraOnboardDevice;

    let entry = |designation, instance, segment, bus| ExtraOnboardDevice {
        device: OnboardDevice {
            designation,
            kind: OnboardType::Other,
        },
        enabled: true,
        instance,
        segment,
        bus,
        devfn: 0,
    };
    let mut out = Vec::with_capacity(3);
    if ambient_light {
        out.push(entry("lightsensor", 35, 2, 0x44));
    }
    out.push(entry("trackpad", 37, 1, 0x67));
    out.push(entry("touchscreen", 38, 2, 0x4a));
    out
}

pub struct Acpi {
    pub devices: Devices,
    pub tpm: Option<Family>,
    pub tpm_irq: Option<fstart_driver_tpm::tis::Irq>,
    pub serial_io: SerialIoMask,
    pub ec_routed: bool,
    pub wlan: bool,
}

impl fstart_platform_intel::acpi::BoardAcpi for crate::Board {
    type Acpi = Acpi;

    fn acpi_baseline() -> Self::Acpi {
        Acpi {
            devices: Devices {
                touchpad_type: 0,
                ambient_light: false,
                lte_ngff: false,
            },
            tpm: Some(Family::Tpm12),
            tpm_irq: None,
            serial_io: SerialIoMask(crate::PLATFORM.southbridge.serialio_enable),
            ec_routed: true,
            wlan: true,
        }
    }
}

const EC: Ec = Ec {
    sci_gpe: 36,
    wake_gpe: Some(0x69),
    mkbp: false,
    pd: false,
    tablet_mode: false,
    keyboard_backlight: false,
};

impl AcpiDevice for Acpi {
    type Config = ();
    fn dsdt_aml(&self, _: &()) -> Vec<u8> {
        let ec = Ec {
            wake_gpe: if self.ec_routed { EC.wake_gpe } else { None },
            ..EC
        };
        let lpc = ec
            .lpc_aml_with_policy(LpcPolicy {
                thermal: Some(ThermalPolicy {
                    notify_target: Some("\\_TZ_"),
                    throttle_method: Some("\\_TZ_.THRT"),
                }),
                power_notify: Some(PowerNotify {
                    method: "\\PNOT",
                    ac_state: "\\PWRS",
                    lid_state: "\\LIDS",
                }),
                external_crec: Some("\\_SB_.CREC"),
                full_battery_workaround: true,
            })
            .expect("Slippy EC");
        let mut pci = fstart_driver_chromeec::acpi::keyboard_aml_with_irq(
            &fstart_acpi::IsaIrq::new(true, true, true, 1),
        );
        match self.tpm {
            Some(Family::Tpm12) => pci.extend(TPM12.dsdt_aml_with(TpmTisOptions {
                // acpi_device_uid(): CRC32 (non-reflected, seed 0) of the PNP
                // device's coreboot path, "\\_SB.PCI0.LPCB.TPM", before SSDT placement.
                uid: Some(0x89e3_7d9c),
                emit_cid: true,
                emit_sta: true,
                io_port: Some((0x0c31, 2)),
                irq: self.tpm_irq.map(|i| TpmIrq {
                    vector: i.vector.into(),
                    edge: i.edge,
                    active_low: i.low,
                }),
            })),
            Some(Family::Tpm20) => pci.extend(TPM20.dsdt_aml()),
            None => (),
        }
        let mut aml = scope_vec(HaswellUlt::LPC.as_str(), &lpc).unwrap();
        aml.extend(scope_vec("\\_SB_.PCI0", &pci).unwrap());
        aml.extend(scope_vec("\\_SB_", &ec.crec().expect("CREC")).unwrap());
        if self.wlan {
            aml.extend_from_slice(
                &acpi_dsl! {Scope("\\_SB_.PCI0.RP01"){Device("WLAN"){Name("_ADR",0u8);}}},
            );
        }
        aml.extend(thermal());
        aml.extend_from_slice(&acpi_dsl! {
            Scope("\\") { Name("PWRS",0u8);Name("LIDS",0u8); Method("_WAK",1,NotSerialized){
                Local0=#{const "\\_SB_.PCI0.LPCB.EC0_.ACEX"};
                If (PWRS != Local0){PWRS=Local0;Notify(#{const "\\_SB_.PCI0.LPCB.EC0_.AC__"},0x80u8);PNOT();}
                Local0=#{const "\\_SB_.PCI0.LPCB.EC0_.LIDS"};
                If (LIDS != Local0){LIDS=Local0;Notify(#{const "\\_SB_.PCI0.LPCB.EC0_.LID0"},0x80u8);}
                Return(Package(0u8,0u8));
            } }
        });
        aml.extend(i2c_devices(self.devices, self.serial_io));
        aml.extend(connectors(self.devices.lte_ngff));
        aml
    }
    fn extra_tables(&self, _: &()) -> Vec<Vec<u8>> {
        if self.tpm == Some(Family::Tpm20) {
            TPM20.tables()
        } else {
            Vec::new()
        }
    }
}

const TPM12: TpmTisAcpi = TpmTisAcpi {
    family: TpmFamily::Tpm12,
    server: false,
    log: None,
};
const TPM20: TpmTisAcpi = TpmTisAcpi {
    family: TpmFamily::Tpm20,
    server: false,
    log: None,
};

/// Slippy thermal.h/acpi/thermal.asl: PECI sensor 0, passive 95 C,
/// critical 99 C. A critical reading must survive a fresh sample.
fn thermal() -> Vec<u8> {
    let mut aml: Vec<u8> = acpi_dsl!{Scope("\\_TZ_") {
        Method("THRT",1,Serialized){
            If (Arg0){#{const "\\_SB_.PCI0.MCHC.CTLE"}(12u8);} Else {#{const "\\_SB_.PCI0.MCHC.CTLD"}();}
        }
        ThermalZone("THRM"){
            Name("_TC1",2u8); Name("_TC2",5u8); Name("_TZP",100u8); Name("_TSP",20u8);
            Name("_PSV",3682u32); Name("_CRT",3722u32);
            Method("_PSL",0,NotSerialized){Return(PPKG());}
            Method("TCHK",0,Serialized){
                Local0 = PECI();
                If (Local0 >= #{const "\\_SB_.PCI0.LPCB.EC0_.TNCA"}) {Return(2732u32);}
                Return((Local0 + #{const "\\_SB_.PCI0.LPCB.EC0_.TOFS"}) * 10u8);
            }
            Method("_TMP",0,Serialized){Local0=TCHK(); If (Local0 >= _CRT){Sleep(1000u32);Local0=TCHK();} Return(Local0);}
        }
    }}.into();
    use fstart_acpi::aml::{Method, MethodCall, Path, Return};
    let sensor = MethodCall::new(Path::new("\\_SB_.PCI0.LPCB.EC0_.TINS"), vec![&0u8]);
    let mut peci = Vec::new();
    Method::new("PECI".into(), 0, true, vec![&Return::new(&sensor)]).to_aml_bytes(&mut peci);
    aml.extend(scope_vec("\\_TZ_.THRM", &peci).unwrap());
    aml
}

const BUS0: &str = "\\_SB_.PCI0.I2C0";
const BUS1: &str = "\\_SB_.PCI0.I2C1";
fn i2c_devices(options: Devices, serial_io: SerialIoMask) -> Vec<u8> {
    let mut aml = Vec::new();
    for (bus, name, hid, desc, uid, addr, irq, wake) in [
        (
            BUS0,
            "ETPA",
            "ELAN0000",
            "Elan Touchpad",
            1u8,
            0x15,
            37,
            Some(12u8),
        ),
        (
            BUS0,
            "CTPA",
            "CYAP0000",
            "Cypress Touchpad",
            3,
            0x67,
            37,
            Some(12),
        ),
        (
            BUS1,
            "ATSA",
            "ATML0001",
            "Atmel Touchscreen",
            5,
            0x4a,
            38,
            Some(25),
        ),
        (
            BUS1,
            "ALSI",
            "ISL29018",
            "Intersil 29018 Ambient Light Sensor",
            6,
            0x44,
            35,
            None,
        ),
    ] {
        let bit = if bus == BUS0 {
            SerialIoMask::I2C0
        } else {
            SerialIoMask::I2C1
        };
        if !serial_io.contains(bit) {
            continue;
        }
        if (name == "ALSI" && !options.ambient_light)
            || (name == "ETPA" && options.touchpad_type == 2)
            || (name == "CTPA" && options.touchpad_type == 1)
        {
            continue;
        }
        let mut body = Vec::new();
        Name::new("_HID".into(), &hid).to_aml_bytes(&mut body);
        Name::new("_DDN".into(), &desc).to_aml_bytes(&mut body);
        Name::new("_UID".into(), &uid).to_aml_bytes(&mut body);
        let connection = I2cSerialBus {
            resource_source: bus,
            slave_address: addr,
            connection_speed: 400_000,
            address_10bit: false,
            consumer: true,
        };
        let pad = bus == BUS0;
        let interrupt = Interrupt::new(true, !pad, true, pad, irq);
        Name::new(
            "_CRS".into(),
            &ResourceTemplate::new(vec![&connection, &interrupt]),
        )
        .to_aml_bytes(&mut body);
        if let Some(pin) = wake {
            acpi_dsl!{
                Name("ISTP",#{byte u8::from(pad)});
                Name("_PRW",Package(#{byte pin},3u8));
                Method("_DSW",3,NotSerialized){If(Arg0 == 1u8){#{const "\\_SB_.PCI0.LPCB.GPIO.GWAK"}(#{byte pin});}}
            }.emit(&mut body).unwrap();
            if name != "ETPA" {
                body.extend_from_slice(&acpi_dsl! {Name("_S0W",4u8);});
            }
        }
        if name == "ATSA" {
            body.extend_from_slice(&acpi_dsl!{
            Name("_STA",0xfu8);Name("GPIO",25u8);
            Name("_DSD",Package(ToUUID("daffd814-6eba-4d8c-8a91-bc9bbf4aa301"),Package(Package("compatible","atmel,maxtouch"))));
        });
        }
        aml.extend(scope_vec(bus, &device_vec(name, &body).unwrap()).unwrap());
    }
    aml
}
fn connectors(lte: bool) -> Vec<u8> {
    let mut aml = Vec::new();
    for (port, kind, visible) in [
        ("PRT1", 0xffu8, Some(false)),
        ("PRT2", 0, Some(true)),
        ("PRT3", 0xff, Some(false)),
        ("PRT4", 0xff, Some(false)),
        ("PRT5", 0, Some(true)),
        ("PRT6", 0xff, Some(true)),
        ("PRT7", 0xff, Some(false)),
        ("SSP1", 3, None),
    ] {
        if port == "PRT6" && !lte {
            continue;
        }
        let mut props = Vec::new();
        acpi_dsl! {Name("_UPC",Package(0xffu8,#{byte kind},0u8,0u8));}
            .emit(&mut props)
            .unwrap();
        if port == "PRT6" {
            use fstart_acpi::aml::{BufferData, Package};
            let mut bytes = vec![0; 20];
            bytes[0] = 0x82; // revision 2, ignore colour
            bytes[8] = 0x71; // visible, unknown panel, vertically centred
            bytes[9] = 0x1d; // horizontally centred, unknown shape
            Name::new("_PLD".into(), &Package::new(vec![&BufferData::new(bytes)]))
                .to_aml_bytes(&mut props);
        } else if let Some(visible) = visible {
            acpi_dsl! {Method("_PLD",0,NotSerialized){Return(GPLD(#{byte u8::from(visible)}));}}
                .emit(&mut props)
                .unwrap();
        }
        aml.extend(scope_vec(&alloc::format!("\\_SB_.PCI0.XHCI.HUB7.{port}"), &props).unwrap());
    }
    aml
}
