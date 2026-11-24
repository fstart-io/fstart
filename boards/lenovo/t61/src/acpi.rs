// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 Alexis Calin
//! T61 tables and NVIDIA G8x panel brightness policy.
extern crate alloc;
use crate::mainboard::{Acpi, Identity};
use alloc::vec::Vec;
use fstart_acpi_macros::acpi_dsl;

pub const DGPU: fstart_acpi::AcpiPath = fstart_acpi::AcpiPath::SB
    .child("PCI0")
    .child("PEGP")
    .child("DGPU");

impl Identity {
    pub fn smbios(&self) -> fstart_acpi::smbios::SmbiosIdentity<'_> {
        fstart_driver_lenovo::thinkpad::smbios_identity(
            SMBIOS,
            self.eeprom.as_ref(),
            self.ec_oem_string.as_ref(),
        )
    }
}

pub static SMBIOS: fstart_acpi::smbios::SmbiosIdentity<'static> =
    fstart_acpi::smbios::SmbiosIdentity {
        bios_vendor: "fstart",
        bios_version: "CBET4000 fstart 0.1.0",
        bios_release_date: fstart_platform_intel::SMBIOS_RELEASE_DATE,
        sys_manufacturer: "LENOVO",
        sys_product: "ThinkPad T61",
        sys_version: "ThinkPad T61",
        sys_serial: None,
        sys_uuid: None,
        bb_manufacturer: "LENOVO",
        bb_product: "ThinkPad T61",
        bb_version: "",
        bb_serial: None,
        chassis_type: 0x0a,
        chassis_manufacturer: "LENOVO",
        processor_sockets: &["Socket P"],
        oem_string: None,
    };

impl fstart_acpi::device::AcpiDevice for Acpi {
    type Config = ();
    fn dsdt_aml(&self, _: &()) -> Vec<u8> {
        dsdt_aml()
    }
}

impl fstart_platform_intel::acpi::BoardAcpi for crate::Board {
    type Acpi = Acpi;

    fn acpi_baseline() -> Self::Acpi {
        Acpi
    }
}

pub fn dsdt_aml() -> Vec<u8> {
    let brightness = acpi_dsl! {
        Scope("\\") {
            Method("BRTU", 0, NotSerialized) {
                If (#{const "\\_SB_.PCI0.MCHC.IGDE"}) { #{const "\\_SB_.PCI0.GFX0.INCB"}(); }
                Else { Notify(#{const "\\_SB_.PCI0.PEGP.DGPU.LCD0"}, 0x86u32); }
            }
            Method("BRTD", 0, NotSerialized) {
                If (#{const "\\_SB_.PCI0.MCHC.IGDE"}) { #{const "\\_SB_.PCI0.GFX0.DECB"}(); }
                Else { Notify(#{const "\\_SB_.PCI0.PEGP.DGPU.LCD0"}, 0x87u32); }
            }
        }
    }
    .to_vec();
    let mut aml = fstart_driver_lenovo::t6::acpi::dsdt_aml_with_brightness::<crate::Hardware>(
        &crate::T61_H8,
        &brightness,
        fstart_driver_lenovo::t6::Model::T61,
    );
    aml.extend(video_aml());
    aml
}

fn video_aml() -> Vec<u8> {
    acpi_dsl! {
        Scope(#{const "\\_SB_.PCI0.PEGP.DGPU"}) {
            Method("_DOS", 1, NotSerialized) {}
            Method("_DOD", 0, NotSerialized) { Return(Package(0x0100u32, 0x0110u32, 0x0120u32)); }
            Device("CRT0") { Name("_ADR", 0x0100u32); }
            Device("DVI0") { Name("_ADR", 0x0120u32); }
            OperationRegion("DGPC", PciConfig, 0u32, 0x14u32);
            Field("DGPC", DWordAcc, NoLock, Preserve) { GPID, 32, Offset(0x10), BAR0, 32, }
            Device("LCD0") {
                Name("_ADR", 0x0110u32);
                Name("BCLP", Package(20u32, 25u32, 30u32, 35u32, 40u32, 45u32, 50u32, 55u32,
                    60u32, 65u32, 70u32, 75u32, 80u32, 85u32, 90u32, 100u32));
                // CCFLs need at least 0x25/255 duty: lower values light only a corner.
                Name("BCLD", Package(0x25u32, 0x4eu32, 0x5cu32, 0x69u32, 0x75u32, 0x81u32, 0x8cu32, 0x97u32,
                    0xa2u32, 0xadu32, 0xb8u32, 0xc6u32, 0xd3u32, 0xe1u32, 0xf0u32, 0xffu32));
                Method("GPON", 0, NotSerialized) {
                    If (#{const "\\_SB_.PCI0.MCHC.PEGE"} == 0u32) { Return(0u32); }
                    Local0 = #{const "\\_SB_.PCI0.PEGP.DGPU.GPID"};
                    If ((Local0 != 0x042910deu32) && (Local0 != 0x040c10deu32)) { Return(0u32); }
                    Return((#{const "\\_SB_.PCI0.PEGP.DGPU.BAR0"} & 0xfffffff0u32) != 0u32);
                }
                // SOR0 PWM on these G8x GPUs. Bit 31 latches a duty out of 1025.
                Method("PWMR", 0, Serialized) {
                    If (GPON() == 0u32) { Return(0u32); }
                    OperationRegion("SPWM", SystemMemory, (#{const "\\_SB_.PCI0.PEGP.DGPU.BAR0"} & 0xfffffff0u32) + 0x61c084u32, 4u32);
                    Field("SPWM", DWordAcc, NoLock, Preserve) { PWMC, 32, }
                    Return(PWMC & 0x7ffu32);
                }
                Method("PWMW", 1, Serialized) {
                    If (GPON() == 0u32) { Return(0u32); }
                    OperationRegion("WPWM", SystemMemory, (#{const "\\_SB_.PCI0.PEGP.DGPU.BAR0"} & 0xfffffff0u32) + 0x61c084u32, 4u32);
                    Field("WPWM", DWordAcc, NoLock, Preserve) { PWMD, 32, }
                    PWMD = 0x80000000u32 | Arg0;
                    Return(0u32);
                }
                Method("_BCL", 0, NotSerialized) {
                    Return(Package(100u32, 100u32, 20u32, 25u32, 30u32, 35u32, 40u32, 45u32,
                        50u32, 55u32, 60u32, 65u32, 70u32, 75u32, 80u32, 85u32, 90u32, 100u32));
                }
                Method("_BCM", 1, NotSerialized) {
                    Local0 = 0u32;
                    While (Local0 < 16u32) {
                        If (DeRefOf(Index(BCLP, Local0)) == Arg0) {
                            PWMW(DeRefOf(Index(BCLD, Local0)) * 1025u32 / 255u32);
                            Return(0u32);
                        }
                        Increment(Local0);
                    }
                    Return(0u32);
                }
                Method("_BQC", 0, NotSerialized) {
                    Local0 = PWMR(); Local1 = 15u32;
                    While (Local1 > 0u32) {
                        If (DeRefOf(Index(BCLD, Local1)) * 1025u32 / 255u32 <= Local0) { Break; }
                        Decrement(Local1);
                    }
                    Return(DeRefOf(Index(BCLP, Local1)));
                }
            }
        }
    }.to_vec()
}
