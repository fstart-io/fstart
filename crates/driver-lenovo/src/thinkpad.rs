//! Pieces the H8-era ThinkPads share across chipsets (X200 on GM45, X201
//! on Ironlake): the PC87382 tablet digitizer and the DSDT glue around the
//! H8 (coreboot `mainboard/lenovo/{x200,x201}`).

#[cfg(feature = "acpi")]
use crate::h8::H8Config;

/// The PC87382 config port. Only its LDN 3 serial port is wired, to the
/// tablet digitizer.
pub const PC87382_BASE: u16 = 0x164e;

/// The devicetree's PC87382: the digitizer on LDN 3 at 0x200/IRQ 5; IR
/// (2), GPIO (7) and DLPC (0x19) are not connected.
pub const PC87382_DEVICES: &[fstart_driver_superio::pc87382::PnpDevice] = {
    use fstart_driver_superio::pc87382::PnpDevice;
    const fn off(ldn: u8) -> PnpDevice {
        PnpDevice {
            ldn,
            enabled: false,
            io: None,
            registers: &[],
        }
    }
    &[
        PnpDevice {
            ldn: 3,
            enabled: true,
            io: Some(0x200),
            registers: &[(0x29, 0xb0), (0x70, 0x05), (0xf0, 0x82)],
        },
        off(2),
        off(7),
        off(0x19),
    ]
};

/// Program the PC87382 like the devicetree and report a tablet (coreboot
/// `drivers/lenovo/wacom.c`): a responding SuperIO and a tablet machine
/// type.
pub fn digitizer_init(part_number: Option<&str>) -> bool {
    fstart_driver_superio::pc87382::configure(PC87382_BASE, PC87382_DEVICES);
    fstart_driver_superio::pc87382::sid(PC87382_BASE) != 0xff
        && part_number.is_some_and(crate::wacom::is_tablet)
}

/// The H8 DSDT plus the root glue these ThinkPads share: hotkey brightness
/// through `\_SB.PCI0.GFX0`, the ThinkLight `\UCMS` method (coreboot
/// `ec/lenovo/h8/acpi/thinklight.asl`), `_PTS`/`_WAK` (`acpi/platform.asl`)
/// and the EC wake GPE `_L18` (`acpi/gpe.asl`).
#[cfg(feature = "acpi")]
#[must_use]
pub fn h8_dsdt_aml<N: fstart_acpi::PcNamespace>(h8: &H8Config) -> alloc::vec::Vec<u8> {
    use crate::h8_acpi::ec;
    use fstart_acpi_macros::acpi_dsl;
    let brightness = acpi_dsl! {
        Scope("\\") {
            // GM45 can hide IGD or leave its MMIO BAR unassigned.
            Method("BRTU", 0, NotSerialized) {
                If (CondRefOf(#{const path 4 N::IGD.child("INCB")})) {
                    #{const path 4 N::IGD.child("INCB")}();
                }
            }
            Method("BRTD", 0, NotSerialized) {
                If (CondRefOf(#{const path 4 N::IGD.child("DECB")})) {
                    #{const path 4 N::IGD.child("DECB")}();
                }
            }
        }
    };
    let mut aml = crate::h8_acpi::dsdt_aml_with_brightness::<N>(h8, &brightness);
    aml.extend_from_slice(&acpi_dsl! {
        Scope("\\") {
            Method("UCMS", 1, Serialized) {
                If (Arg0 == 0x0Cu32) {
                    #{const path 5 ec::<N>().child("LGHT")}(1u32);
                }
                If (Arg0 == 0x0Du32) {
                    #{const path 5 ec::<N>().child("LGHT")}(0u32);
                }
            }
            Method("_PTS", 1, NotSerialized) {
                #{const path 5 ec::<N>().child("MUTE")}(1u32);
                #{const path 5 ec::<N>().child("USBP")}(0u32);
                #{const path 5 ec::<N>().child("RADI")}(0u32);
            }
            Method("_WAK", 1, NotSerialized) {
                #{const path 6 ec::<N>().child("HKEY").child("WAKE")}(Arg0);
                Return(Package(0u32, 0u32));
            }
        }
        Scope("\\_GPE") {
            Method("_L18", 0, NotSerialized) {
                If (#{const path 5 ec::<N>().child("WAKE")}) {}
            }
        }
    });
    aml
}

/// Apply ThinkPad AT24RF08C EEPROM identity and H8 EC OEM string onto a
/// board-static SMBIOS identity. Matches coreboot's
/// `drivers/i2c/at24rf08c/lenovo_serials.c`, where EEPROM fields populate both
/// system and baseboard entries.
#[cfg(feature = "acpi")]
#[must_use]
pub fn smbios_identity<'a>(
    base: fstart_acpi::smbios::SmbiosIdentity<'static>,
    eeprom: Option<&'a crate::eeprom::Identity>,
    ec_oem: Option<&'a crate::h8::EcOemString>,
) -> fstart_acpi::smbios::SmbiosIdentity<'a> {
    let mut identity = base;
    if let Some(eeprom) = eeprom {
        identity.sys_product = eeprom.part_number();
        identity.sys_version = eeprom.version();
        identity.sys_serial = Some(eeprom.serial_number());
        identity.sys_uuid = Some(eeprom.uuid);
        identity.bb_product = eeprom.part_number();
        identity.bb_version = eeprom.version();
        identity.bb_serial = Some(eeprom.serial_number());
    }
    identity.oem_string = ec_oem.map(|oem| oem.as_str());
    identity
}

#[cfg(feature = "acpi")]
extern crate alloc;
