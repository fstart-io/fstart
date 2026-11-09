//! Lenovo ThinkPad X201 mainboard work the stage `main`s call: the EHCI
//! debug console, the clock generator, H8/PMH7 policy, the dock
//! connection, the identity EEPROM and the X201T digitizer.

#[cfg(any(feature = "acpi", test))]
extern crate alloc;

#[cfg(any(feature = "acpi", test))]
use fstart_acpi::PcNamespace;
use fstart_core::services::{ServiceError, SmBus};
use fstart_driver_intel::southbridge::gpio_ich::{IchGpio, IchPin};
use fstart_driver_lenovo::x201;
#[cfg(any(feature = "acpi", test))]
use fstart_platform_intel::ironlake::IronlakeIbexPeak;

/// The EHCI debug port of EHCI2 (00:1a.0): coreboot's X201
/// `USBDEBUG_HCD_INDEX=2`.
pub const fn console() -> fstart_driver_usbdebug::Config {
    crate::config::EHCI_CONSOLE
}

/// coreboot `bootblock_mainboard_early_init()`: the EC powers the USB
/// ports before the debug console needs one.
pub fn early_usb_power() {
    // No console yet to report a failure; the console init reports a dead
    // port instead.
    let _ = x201::early_usb_power(&crate::config::X201_H8);
}

/// coreboot `set_fsb_frequency()` from `mainboard_pre_raminit()`: the
/// clock generator's first two registers hold the FSB word. Like coreboot,
/// the first five registers are written back.
pub fn set_fsb_frequency(smbus: &mut impl SmBus) -> Result<(), ServiceError> {
    let mut block = [0u8; 32];
    let count = smbus.block_read(crate::config::CLOCKGEN_ADDRESS, 0, &mut block)?;
    if count < 5 {
        return Err(ServiceError::HardwareError);
    }
    block[..2].copy_from_slice(&crate::config::CLOCKGEN_FSB_FREQUENCY.to_le_bytes());
    smbus.block_write(crate::config::CLOCKGEN_ADDRESS, 0, &block[..5])
}

/// The PCH GPIOs the board's own devices hang off.
pub struct Pins {
    /// Bluetooth daughter card fitted (active low).
    pub bluetooth_card: IchPin,
    /// Dock ID straps GPIO3..5; 7 means no dock.
    pub dock_id: [IchPin; 3],
    /// Connects a present dock.
    pub dock_connect: IchPin,
}

impl Pins {
    pub const fn of(gpio: IchGpio) -> Self {
        let [a, b, c] = x201::DOCK_ID_GPIOS;
        Self {
            bluetooth_card: gpio.pin(x201::BDC_PRESENCE_GPIO),
            dock_id: [gpio.pin(a), gpio.pin(b), gpio.pin(c)],
            dock_connect: gpio.pin(x201::DOCK_CONNECT_GPIO),
        }
    }
}

/// What the board read from its own hardware for the tables.
#[derive(Default)]
pub struct Identity {
    eeprom: Option<fstart_driver_lenovo::eeprom::Identity>,
    ec_oem_string: Option<fstart_driver_lenovo::h8::EcOemString>,
}

#[cfg(any(feature = "acpi", test))]
impl Identity {
    /// The SMBIOS identity: like coreboot (`at24rf08c/lenovo_serials.c`),
    /// the EEPROM fields describe both system and board.
    pub fn smbios(&self) -> fstart_acpi::smbios::SmbiosIdentity<'_> {
        let mut identity = X201_SMBIOS_IDENTITY;
        if let Some(eeprom) = &self.eeprom {
            identity.sys_product = eeprom.part_number();
            identity.sys_version = eeprom.version();
            identity.sys_serial = Some(eeprom.serial_number());
            identity.sys_uuid = Some(eeprom.uuid);
            identity.bb_product = eeprom.part_number();
            identity.bb_version = eeprom.version();
            identity.bb_serial = Some(eeprom.serial_number());
        }
        identity.oem_string = self.ec_oem_string.as_ref().map(|oem| oem.as_str());
        identity
    }
}

/// EC/PMH7, the dock, the identity EEPROM and the digitizer, once the
/// chipset devices are up. Restores the optional TPM on S3 before returning
/// the identity and the board ACPI device.
#[cfg(any(feature = "acpi", test))]
pub fn init_devices(
    pins: &mut Pins,
    smbus: &mut impl SmBus,
    resume: bool,
) -> Result<(Identity, Acpi), fstart_driver_tpm::Error> {
    let mut identity = Identity::default();
    let (bluetooth, acknowledged) = x201::ec_init(
        &mut pins.bluetooth_card,
        &mut pins.dock_id,
        &mut pins.dock_connect,
        &crate::config::X201_H8,
        resume,
    );
    if !acknowledged {
        fstart_log::error!("lenovo-x201: H8 EC initialization incomplete");
    }
    if !resume {
        // The AT24RF08C shares the SPD bus; read-only identity access never
        // touches its data or protection registers.
        identity.eeprom = fstart_driver_lenovo::eeprom::Identity::read(smbus)
            .inspect_err(|error| {
                fstart_log::error!(
                    "lenovo-x201: EEPROM identity unavailable, code {}",
                    *error as u8
                )
            })
            .ok();
        identity.ec_oem_string = fstart_driver_lenovo::h8::H8::new(
            fstart_driver_lenovo::ec::Ec::new(crate::config::X201_H8.resources.os),
        )
        .smbios_oem_string();
        if identity.ec_oem_string.is_none() {
            fstart_log::error!("lenovo-x201: H8 EC firmware id unavailable");
        }
    }
    // The digitizer's SuperIO is programmed on every boot; whether it is a
    // tablet only matters for the tables a cold boot builds.
    let digitizer = fstart_driver_lenovo::thinkpad::digitizer_init(
        identity.eeprom.as_ref().map(|e| e.part_number()),
    );
    Ok((
        identity,
        Acpi {
            bluetooth,
            digitizer,
            tpm: init_tpm(resume)?,
        },
    ))
}

/// Identify the optional LPC TIS TPM and restore its saved state on S3.
/// Cold boots leave startup to the payload, as before.
#[cfg(any(feature = "acpi", test))]
fn init_tpm(
    resume: bool,
) -> Result<Option<fstart_acpi::devices::tpm::TpmFamily>, fstart_driver_tpm::Error> {
    use fstart_acpi::devices::tpm::TpmFamily;
    use fstart_driver_tpm::{Family, tis};
    // SAFETY: the PCH decodes the TIS window to LPC; nothing else drives it.
    let mut tis = unsafe { tis::Tis::new(tis::BASE, fstart_arch::x86::Udelay) };
    let Some(id) = tis.probe() else {
        return Ok(None);
    };
    fstart_log::info!(
        "lenovo-x201: TPM {:#x}:{:#x} revision {}",
        id.vendor,
        id.device,
        id.revision
    );
    if resume {
        fstart_driver_tpm::resume(&mut tis, id.family)?;
        fstart_log::info!("lenovo-x201: TPM S3 state restored");
    }
    Ok(Some(match id.family {
        Family::Tpm12 => TpmFamily::Tpm12,
        Family::Tpm20 => TpmFamily::Tpm20,
    }))
}

/// Route EC events to the installed SMM handler until OSPM enables ACPI.
/// Call only on cold boot: S3 must retain the OS's SCI ownership.
pub fn legacy_ec_events() {
    use fstart_driver_intel::southbridge::smi::{ICH8_GPE0, IchSmi};
    let smi = IchSmi::new(fstart_platform_intel::ironlake::IBEXPEAK_PMBASE, ICH8_GPE0);
    // SAFETY: the BSP owns PCI config after MP/SMM installation completes.
    unsafe { smi.route_gpi(x201::EC_SCI_GPI, false) };
}

/// The board's ACPI: H8 EC surface, sleep hooks, ThinkLight, dock, the
/// keyboard controller, the X201T digitizer and the TPM.
#[cfg(any(feature = "acpi", test))]
pub struct Acpi {
    bluetooth: bool,
    digitizer: bool,
    tpm: Option<fstart_acpi::devices::tpm::TpmFamily>,
}

#[cfg(any(feature = "acpi", test))]
impl fstart_acpi::device::AcpiDevice for Acpi {
    type Config = ();

    fn dsdt_aml(&self, _: &Self::Config) -> alloc::vec::Vec<u8> {
        let mut aml = x201_mainboard_dsdt_aml(self.bluetooth, self.digitizer);
        if let Some(family) = self.tpm {
            // coreboot `lpc_tpm_fill_ssdt()` places it under `\_SB_.PCI0`.
            let tpm = fstart_acpi::devices::tpm::TpmTisAcpi {
                family,
                server: false,
                log: None,
            };
            aml.extend(
                fstart_acpi::aml_linker::scope_vec("\\_SB_.PCI0", &tpm.dsdt_aml())
                    .expect("X201 TPM emission"),
            );
        }
        aml
    }
}

/// Assemble the X201 DSDT.
#[cfg(any(feature = "acpi", test))]
pub fn x201_mainboard_dsdt_aml(bluetooth: bool, digitizer: bool) -> alloc::vec::Vec<u8> {
    let h8 = fstart_driver_lenovo::h8::H8Config {
        has_bluetooth: bluetooth,
        ..crate::config::X201_H8
    };
    let mut aml = x201::acpi::dsdt_aml::<fstart_platform_intel::ironlake::IronlakeIbexPeak>(
        &h8,
        fstart_platform_intel::ironlake::IBEXPEAK_GPIOBASE,
        digitizer,
    );
    // Kconfig PS2K_EISAID/PS2M_EISAID: the TrackPoint is LEN0018.
    aml.extend(
        fstart_acpi::aml_linker::scope_vec(
            IronlakeIbexPeak::LPC.as_str(),
            &fstart_driver_intel::southbridge::acpi::ps2_controller_nodes_with(
                "PNP0303", "LEN0018",
            ),
        )
        .expect("X201 PS/2 emission"),
    );
    aml
}

#[cfg(any(feature = "acpi", test))]
static X201_SMBIOS_PROCESSOR_SOCKETS: [&str; 1] = ["BGA1288"];

#[cfg(any(feature = "acpi", test))]
pub static X201_SMBIOS_IDENTITY: fstart_acpi::smbios::SmbiosIdentity<'static> =
    fstart_acpi::smbios::SmbiosIdentity {
        bios_vendor: "fstart",
        // Non-OEM firmware ID, following coreboot and the X200 binding;
        // Linux thinkpad_acpi validates this before the ThinkPad model string.
        bios_version: "CBET4000 fstart 0.1.0",
        bios_release_date: fstart_platform_intel::SMBIOS_RELEASE_DATE,
        sys_manufacturer: "LENOVO",
        sys_product: "ThinkPad X201",
        sys_version: "ThinkPad X201",
        sys_serial: None,
        sys_uuid: None,
        bb_manufacturer: "LENOVO",
        bb_product: "ThinkPad X201",
        bb_version: "1.0",
        bb_serial: None,
        chassis_type: 0x0a,
        chassis_manufacturer: "LENOVO",
        processor_sockets: &X201_SMBIOS_PROCESSOR_SOCKETS,
        oem_string: None,
    };

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_acpi::device::AcpiDevice;
    use fstart_driver_intel::ibexpeak::IntelIbexPeak;
    use fstart_driver_intel::ironlake::IntelIronlake;
    use fstart_driver_intel::namespace::SouthbridgeAcpi;
    use fstart_driver_intel::{IntelNorthbridgeDriver, IntelSouthbridgeDriver};
    use std::{fs, process::Command};

    fn iasl(dir: &std::path::Path, arguments: &[&str]) {
        let output = Command::new("iasl")
            .current_dir(dir)
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "iasl {arguments:?} failed: {}{}",
            std::string::String::from_utf8_lossy(&output.stdout),
            std::string::String::from_utf8_lossy(&output.stderr)
        );
    }

    /// The chipset and board DSDT parts disassemble and recompile, and
    /// carry coreboot's X201 surface (dock, EC wake, PS/2 IDs).
    #[test]
    fn complete_dsdt_iasl_round_trip() {
        let north_config = &crate::X201_PLATFORM.northbridge;
        let south_config = &crate::X201_PLATFORM.southbridge;
        let north = IntelIronlake::new_from_config(north_config).unwrap();
        let south = IntelIbexPeak::new_from_config(south_config).unwrap();

        let mut aml = north.dsdt_aml(north_config);
        aml.extend(south.dsdt_aml(
            south_config,
            &fstart_driver_intel::namespace::pci_root::<IntelIronlake>(),
        ));
        aml.extend(
            Acpi {
                bluetooth: true,
                digitizer: false,
                tpm: Some(fstart_acpi::devices::tpm::TpmFamily::Tpm12),
            }
            .dsdt_aml(&()),
        );
        let dsdt = fstart_acpi::platform::build_dsdt(&aml);

        let dir =
            std::env::temp_dir().join(std::format!("fstart-x201-dsdt-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("dsdt.aml"), &dsdt).unwrap();
        iasl(&dir, &["-d", "dsdt.aml"]);
        let source = fs::read_to_string(dir.join("dsdt.dsl")).unwrap();
        for method in [
            "_Q18", "_Q37", "_Q45", "_Q50", "_Q58", "_DCK", "_L18", "UCMS",
        ] {
            assert!(
                source.contains(&std::format!("Method ({method},")),
                "missing {method}"
            );
        }
        assert!(source.contains("EisaId (\"LEN0018\")"));
        assert!(source.contains("EisaId (\"PNP0C31\")"));
        assert!(source.contains("GFX0.INCB ()"));
        assert!(source.contains("Name (_GPE, 0x11)"));
        fs::rename(dir.join("dsdt.aml"), dir.join("original.aml")).unwrap();
        iasl(&dir, &["-oa", "-tc", "dsdt.dsl"]);
        fs::remove_dir_all(dir).unwrap();
    }

    /// A clock generator answering with `count` registers.
    struct Clockgen {
        registers: [u8; 8],
        count: usize,
        written: std::vec::Vec<u8>,
    }

    impl SmBus for Clockgen {
        fn read_byte(&mut self, _: u8, _: u8) -> Result<u8, ServiceError> {
            Err(ServiceError::NotSupported)
        }
        fn write_byte(&mut self, _: u8, _: u8, _: u8) -> Result<(), ServiceError> {
            Err(ServiceError::NotSupported)
        }
        fn block_read(&mut self, _: u8, _: u8, data: &mut [u8]) -> Result<usize, ServiceError> {
            data[..self.count].copy_from_slice(&self.registers[..self.count]);
            Ok(self.count)
        }
        fn block_write(&mut self, _: u8, _: u8, data: &[u8]) -> Result<(), ServiceError> {
            self.written = data.to_vec();
            Ok(())
        }
    }

    #[test]
    fn fsb_word_replaces_the_first_two_of_five_registers() {
        let mut clockgen = Clockgen {
            registers: [1, 2, 3, 4, 5, 6, 7, 8],
            count: 8,
            written: std::vec::Vec::new(),
        };
        set_fsb_frequency(&mut clockgen).unwrap();
        assert_eq!(clockgen.written, [0x9f, 0xf5, 3, 4, 5]);

        clockgen.count = 4;
        assert_eq!(
            set_fsb_frequency(&mut clockgen),
            Err(ServiceError::HardwareError)
        );
    }
}
