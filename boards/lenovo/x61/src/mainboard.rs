//! Lenovo ThinkPad X61 mainboard work the stage `main`s call.
//!
//! This module holds the board-specific parts that do not belong in the
//! reusable GM965 northbridge or ICH8 southbridge drivers. The important
//! pre-console path is the X6 UltraBase dock: ICH8 opens the LPC/GPIO decode
//! windows, then [`setup_dock_console`] initializes the laptop-side DLPC,
//! connects the dock-side LPC bus when present, and enables the dock PC87392
//! COM1 before the NS16550 console driver probes port 0x3f8.

#![allow(clippy::result_unit_err)]

extern crate alloc;

use embedded_hal::digital::{InputPin, OutputPin, PinState};
use fstart_core::services::{ServiceError, SmBus};
use fstart_driver_intel::southbridge::gpio_ich::{IchGpio, IchPin};
use fstart_driver_lenovo::x6::dock as x6_dock;
use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};
use fstart_platform_intel::gm965::Gm965Ich8AcpiContext;

/// The dock's COM1.
pub const fn console() -> Ns16550Config {
    Ns16550Config {
        regs: AccessMode::Pio {
            base: crate::UART0_PIO_BASE as u64,
        },
        clock_freq: crate::UART0_CLOCK_FREQ,
        baud_rate: crate::UART0_BAUD_RATE,
    }
}

/// The ICH8 GPIOs the board's own devices hang off.
pub struct Pins {
    /// Dock attached (`SLICE_ON_3M#`, active low).
    pub dock_present: IchPin,
    /// Bluetooth daughter card fitted (active low).
    pub bluetooth_card: IchPin,
    /// SMBus mux: high reaches the SPDs and the CK505, low the EEPROM.
    pub smbus_mux: IchPin,
}

impl Pins {
    pub const fn of(gpio: IchGpio) -> Self {
        Self {
            dock_present: gpio.pin(x6_dock::DOCK_PRESENT_GPIO),
            bluetooth_card: gpio.pin(fstart_driver_lenovo::x6::BDC_PRESENCE_GPIO),
            smbus_mux: gpio.pin(crate::config::X61_SMBUS_MUX.pin),
        }
    }
}

/// Route the console through the dock before the UART is probed.
pub fn setup_dock_console(dock_present: &mut impl InputPin) {
    x6_dock::setup_console(dock_present, os_ec());
}

/// The firmware's legacy EC channel (the OS owns it once ACPI is enabled).
fn os_ec() -> fstart_driver_lenovo::ec::Ec {
    fstart_driver_lenovo::ec::Ec::new(crate::config::X61_H8.resources.os)
}

/// The branches of the X61's SMBus mux (GPIO 42).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SmbusBranch {
    /// The SPDs and the CK505 clock generator.
    SpdAndClock,
    /// The identity EEPROM.
    Eeprom,
}

/// Switch the SMBus mux ([`Pins::smbus_mux`]). The chipset GPIO table
/// leaves it on [`SmbusBranch::Eeprom`] in every stage.
pub fn select_smbus(mux: &mut impl OutputPin, branch: SmbusBranch) -> Result<(), ServiceError> {
    mux.set_state(PinState::from(branch == SmbusBranch::SpdAndClock))
        .map_err(|_| ServiceError::IoError)
}

/// Whether the UltraBay holds a disk the primary IDE channel can reach. An
/// absent or disconnected dock cannot supply one.
pub fn ultrabay_populated(dock_present: &mut impl InputPin) -> bool {
    x6_dock::dock_present(dock_present) && x6_dock::ultrabay_present()
}

/// What the board read from its own hardware for the tables.
#[derive(Default)]
pub struct Identity {
    eeprom: Option<fstart_driver_lenovo::eeprom::Identity>,
    ec_oem_string: Option<fstart_driver_lenovo::h8::EcOemString>,
}

impl Identity {
    /// The SMBIOS identity: like coreboot, the EEPROM fields describe both
    /// system and board.
    pub fn smbios(&self) -> fstart_acpi::smbios::SmbiosIdentity<'_> {
        let mut identity = X61_SMBIOS_IDENTITY;
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

/// EC/PMH7, dock power, the identity EEPROM and the SD slot, once the
/// chipset devices are up. `smbus` is on the EEPROM branch. EC setup is
/// independent of ACPI table emission. Returns what the tables need on a
/// cold boot.
pub fn init_devices(
    bluetooth_card: &mut impl InputPin,
    smbus: &mut impl SmBus,
    resume: bool,
) -> Identity {
    let mut identity = Identity::default();
    if !fstart_driver_lenovo::x6::ec_init(bluetooth_card, &crate::config::X61_H8, resume) {
        fstart_log::error!("lenovo-x61: H8 EC initialization incomplete");
    }
    if !resume {
        identity.ec_oem_string = fstart_driver_lenovo::x6::ec_oem_string(&crate::config::X61_H8);
        if identity.ec_oem_string.is_none() {
            fstart_log::error!("lenovo-x61: H8 EC firmware id unavailable");
        }
    }
    x6_dock::mainstage_power_policy(os_ec());
    if !resume {
        // Read-only identity access must not modify EEPROM/RFID protection
        // registers.
        identity.eeprom = match fstart_driver_lenovo::eeprom::Identity::read(smbus) {
            Ok(identity) => Some(identity),
            Err(error) => {
                fstart_log::error!(
                    "lenovo-x61: EEPROM identity unavailable, code {}",
                    error as u8
                );
                None
            }
        };
    }
    if ricoh_sd_write_protect().is_err() {
        fstart_log::error!("lenovo-x61: Ricoh SD policy programming failed");
    }
    identity
}

/// Hand EC event routing to its owner. The shared SMM installer sets SCI_EN
/// directly, without an APMC; once permanent SMM is installed, initialize
/// board EC routing through its ACPI command handler (keeping ACPI
/// ownership on resume).
pub fn handoff_ec_events(resume: bool) {
    use fstart_driver_intel::southbridge::smi::{
        APM_CNT, APM_CNT_ACPI_DISABLE, APM_CNT_ACPI_ENABLE,
    };
    let command = if resume {
        APM_CNT_ACPI_ENABLE
    } else {
        APM_CNT_ACPI_DISABLE
    };
    // SAFETY: permanent SMM is installed and owns this command port.
    unsafe { fstart_core::pio::outb(APM_CNT, command) };
}

/// Program the CK505 clock generator. It sits on the SPD branch, so the
/// bootblock does this while that branch is selected for DRAM training.
pub fn init_ck505(bus: &mut impl SmBus) -> Result<(), ServiceError> {
    use fstart_driver_intel::generic::ck505::I2cCk505;

    let mut clock = I2cCk505::new_at_address(crate::config::x61_ck505_config(), 0x69)
        .map_err(|_| ServiceError::HardwareError)?;
    clock
        .init_on_smbus(bus)
        .map_err(|_| ServiceError::HardwareError)
}

/// The onboard SD function is device 0, function 2 behind ICH8's PCI bridge.
/// Enumeration assigns the bus number; the board owns the location/polarity,
/// and the R5C822 helper verifies the identity before touching vendor registers.
fn ricoh_sd_write_protect() -> Result<(), ServiceError> {
    use fstart_driver_intel::ich8::ich8::{PCI_BRIDGE_DEV, PCI_BRIDGE_FUNC};
    use fstart_pci::{EcamDevice, PciType1Config};
    use tock_registers::interfaces::Readable;

    let bridge = EcamDevice::new(0, PCI_BRIDGE_DEV, PCI_BRIDGE_FUNC);
    // SAFETY: the enumerated ICH8 PCI bridge has a mapped Type 1 header.
    let bus = unsafe { bridge.regs::<PciType1Config>() }
        .secondary_bus
        .get();
    if bus == 0 || bus == 0xff {
        return Err(ServiceError::HardwareError);
    }
    fstart_driver_ricoh::r5c822::configure_sd_write_protect(EcamDevice::new(bus, 0, 2), true)
}

/// The board's ACPI: TCO dock commands, sleep/wake hooks, dock, GPE routing
/// and the complete H8 EC surface from the Lenovo driver.
pub struct Acpi;

impl fstart_acpi::device::AcpiDevice for Acpi {
    type Config = Gm965Ich8AcpiContext;

    fn dsdt_aml(&self, config: &Self::Config) -> alloc::vec::Vec<u8> {
        x61_mainboard_dsdt_aml(*config)
    }
}

/// Assemble the X61 DSDT.
pub fn x61_mainboard_dsdt_aml(context: Gm965Ich8AcpiContext) -> alloc::vec::Vec<u8> {
    fstart_driver_lenovo::x6::acpi::dsdt_aml(&crate::config::X61_H8, context.lpc_scope())
}

static X61_SMBIOS_PROCESSOR_SOCKETS: [&str; 1] = ["Socket M"];

pub static X61_SMBIOS_IDENTITY: fstart_acpi::smbios::SmbiosIdentity<'static> =
    fstart_acpi::smbios::SmbiosIdentity {
        bios_vendor: "fstart",
        // Linux thinkpad_acpi requires a Lenovo-shaped firmware ID before
        // inspecting the product version. Use coreboot's non-OEM ID, retaining
        // our real firmware identity rather than impersonating a Lenovo BIOS.
        bios_version: "CBET4000 fstart 0.1.0",
        bios_release_date: fstart_platform_intel::SMBIOS_RELEASE_DATE,
        sys_manufacturer: "LENOVO",
        sys_product: "ThinkPad X61",
        sys_version: "ThinkPad X61",
        sys_serial: None,
        sys_uuid: None,
        bb_manufacturer: "LENOVO",
        bb_product: "ThinkPad X61",
        bb_version: "",
        bb_serial: None,
        chassis_type: 0x0a,
        chassis_manufacturer: "LENOVO",
        processor_sockets: &X61_SMBIOS_PROCESSOR_SOCKETS,
        oem_string: None,
    };

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_acpi::device::AcpiDevice;
    use fstart_driver_intel::gm965::IntelGm965;
    use fstart_driver_intel::ich8::IntelIch8;
    use fstart_driver_intel::{IntelNorthbridgeDriver, IntelSouthbridgeDriver};
    use std::fs;
    use std::process::Command;

    #[derive(Default)]
    struct Bus {
        block: [u8; 4],
        fail_transfer: bool,
        transfers: usize,
    }

    /// Levels an output pin was driven to.
    #[derive(Default)]
    struct Levels(std::vec::Vec<bool>);

    impl embedded_hal::digital::ErrorType for Levels {
        type Error = core::convert::Infallible;
    }

    impl OutputPin for Levels {
        fn set_low(&mut self) -> Result<(), Self::Error> {
            self.0.push(false);
            Ok(())
        }
        fn set_high(&mut self) -> Result<(), Self::Error> {
            self.0.push(true);
            Ok(())
        }
    }

    impl fstart_core::services::SmBus for Bus {
        fn read_byte(&mut self, _: u8, _: u8) -> Result<u8, ServiceError> {
            Err(ServiceError::NotSupported)
        }
        fn write_byte(&mut self, _: u8, _: u8, _: u8) -> Result<(), ServiceError> {
            Err(ServiceError::NotSupported)
        }
        fn block_read(&mut self, _: u8, _: u8, data: &mut [u8]) -> Result<usize, ServiceError> {
            self.transfers += 1;
            if self.fail_transfer {
                return Err(ServiceError::IoError);
            }
            data[..4].copy_from_slice(&self.block);
            Ok(4)
        }
        fn block_write(&mut self, _: u8, _: u8, data: &[u8]) -> Result<(), ServiceError> {
            self.block.copy_from_slice(data);
            Ok(())
        }
    }

    #[test]
    fn smbus_mux_reaches_the_spds_high_and_the_eeprom_low() {
        let mut mux = Levels::default();
        select_smbus(&mut mux, SmbusBranch::SpdAndClock).unwrap();
        select_smbus(&mut mux, SmbusBranch::Eeprom).unwrap();
        assert_eq!(mux.0, [true, false]);
        assert_eq!(crate::config::X61_SMBUS_MUX.pin, 42);
        // The chipset GPIO table leaves every stage on the EEPROM branch.
        assert!(matches!(
            crate::config::X61_SMBUS_MUX.level,
            fstart_driver_intel::southbridge::gpio_ich::GpioLevel::Low
        ));
    }

    #[test]
    fn ck505_programming_reports_transfer_failures() {
        for fail_transfer in [false, true] {
            let mut bus = Bus {
                block: [0x04, 0xa5, 0x5a, 0xff],
                fail_transfer,
                ..Bus::default()
            };
            let expected = if fail_transfer {
                Err(ServiceError::HardwareError)
            } else {
                Ok(())
            };
            assert_eq!(init_ck505(&mut bus), expected);
            assert!(bus.transfers > 0);
        }
    }

    #[test]
    fn complete_dsdt_iasl_round_trip() {
        let north_config = &crate::X61_PLATFORM.northbridge;
        let south_config = &crate::X61_PLATFORM.southbridge;
        let north = IntelGm965::new_from_config(north_config).unwrap();
        let south = IntelIch8::new_from_config(south_config).unwrap();

        let mut aml = north.dsdt_aml(north_config);
        aml.extend(south.dsdt_aml(south_config));
        aml.extend(x61_mainboard_dsdt_aml(Gm965Ich8AcpiContext));
        let dsdt = fstart_acpi::platform::build_dsdt(&aml);

        let dir = std::env::temp_dir().join(std::format!("fstart-x61-dsdt-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("dsdt.aml"), &dsdt).unwrap();

        let disassemble = Command::new("iasl")
            .current_dir(&dir)
            .args(["-d", "dsdt.aml"])
            .output()
            .unwrap();
        assert!(
            disassemble.status.success(),
            "iasl -d failed: {}{}",
            std::string::String::from_utf8_lossy(&disassemble.stdout),
            std::string::String::from_utf8_lossy(&disassemble.stderr)
        );
        let source = fs::read_to_string(dir.join("dsdt.dsl")).unwrap();
        for method in ["_Q18", "_Q37", "_Q50", "_Q58", "BRTU", "BRTD"] {
            assert!(source.contains(&std::format!("Method ({method},")));
        }
        for (method, event) in [("_Q37", "Zero"), ("_Q58", "Zero"), ("_Q50", "0x03")] {
            let body = source
                .split(&std::format!("Method ({method},"))
                .nth(1)
                .unwrap()
                .split('}')
                .next()
                .unwrap();
            assert!(body.contains(&std::format!("Notify (\\_SB.DOCK, {event})")));
        }
        assert!(source.contains("HKEY.RHK (0x09)"));
        assert!(source.contains("GFX0.INCB ()"));
        assert!(source.contains("GFX0.DECB ()"));
        assert!(!source.contains("SMIF"));
        assert!(!source.contains("Method (TRAP,"));
        fs::rename(dir.join("dsdt.aml"), dir.join("original.aml")).unwrap();
        let compile = Command::new("iasl")
            .current_dir(&dir)
            .args(["-oa", "-tc", "dsdt.dsl"])
            .output()
            .unwrap();
        assert!(
            compile.status.success(),
            "iasl -tc failed: {}{}",
            std::string::String::from_utf8_lossy(&compile.stdout),
            std::string::String::from_utf8_lossy(&compile.stderr)
        );

        // ACPICA narrows pre-existing shared Intel integer encodings on
        // the first pass. Once canonicalized, require stable AML bytes.
        let canonical = fs::read(dir.join("dsdt.aml")).unwrap();
        fs::rename(dir.join("dsdt.dsl"), dir.join("original.dsl")).unwrap();
        for arguments in [&["-d", "dsdt.aml"][..], &["-oa", "-tc", "dsdt.dsl"][..]] {
            let output = Command::new("iasl")
                .current_dir(&dir)
                .args(arguments)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "canonical roundtrip failed: {}{}",
                std::string::String::from_utf8_lossy(&output.stdout),
                std::string::String::from_utf8_lossy(&output.stderr)
            );
        }
        let recompiled = fs::read(dir.join("dsdt.aml")).unwrap();
        // Ignore only compiler-identification fields in the table header.
        assert_eq!(&recompiled[36..], &canonical[36..]);
        fs::remove_dir_all(dir).unwrap();
    }
}
