//! Lenovo ThinkPad X60 mainboard work called by the stage mains.

use embedded_hal::digital::InputPin;
use fstart_core::services::{ServiceError, SmBus};
use fstart_driver_intel::southbridge::gpio_ich::{IchGpio, IchPin};
use fstart_driver_lenovo::x6::dock;
use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};
use fstart_platform_intel::i945::I945Ich7AcpiContext;

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

/// GPIOs used by the X6 dock and Bluetooth daughter card.
pub struct Pins {
    /// Dock attached (`SLICE_ON_3M#`, active low).
    pub dock_present: IchPin,
    /// Bluetooth daughter card fitted (active low).
    pub bluetooth_card: IchPin,
}

impl Pins {
    pub const fn of(gpio: IchGpio) -> Self {
        Self {
            dock_present: gpio.pin(dock::DOCK_PRESENT_GPIO),
            bluetooth_card: gpio.pin(fstart_driver_lenovo::x6::BDC_PRESENCE_GPIO),
        }
    }
}

/// Route the console through the dock before the UART is probed.
pub fn setup_dock_console(dock_present: &mut impl InputPin) {
    dock::setup_console(dock_present, os_ec());
}

fn os_ec() -> fstart_driver_lenovo::ec::Ec {
    fstart_driver_lenovo::ec::Ec::new(crate::config::X60_H8.resources.os)
}

/// Whether the UltraBay holds a disk the primary IDE channel can reach.
pub fn ultrabay_populated(dock_present: &mut impl InputPin) -> bool {
    dock::dock_present(dock_present) && dock::ultrabay_present()
}

#[derive(Default)]
pub struct Identity {
    eeprom: Option<fstart_driver_lenovo::eeprom::Identity>,
    ec_oem_string: Option<fstart_driver_lenovo::h8::EcOemString>,
}

impl Identity {
    pub fn smbios(&self) -> fstart_acpi::smbios::SmbiosIdentity<'_> {
        let mut identity = X60_SMBIOS_IDENTITY;
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

/// Initialize EC, dock, clock, EEPROM and SD policy in their original order.
pub fn init_devices(
    bluetooth_card: &mut impl InputPin,
    smbus: &mut impl SmBus,
    resume: bool,
) -> Identity {
    let mut identity = Identity::default();
    if !fstart_driver_lenovo::x6::ec_init(bluetooth_card, &crate::config::X60_H8, resume) {
        fstart_log::error!("lenovo-x60: H8 EC initialization incomplete");
    }
    if !resume {
        identity.ec_oem_string = fstart_driver_lenovo::x6::ec_oem_string(&crate::config::X60_H8);
        if identity.ec_oem_string.is_none() {
            fstart_log::error!("lenovo-x60: H8 EC firmware id unavailable");
        }
    }
    dock::mainstage_power_policy(os_ec());
    if !resume {
        if init_ck505(smbus).is_err() {
            fstart_log::error!("lenovo-x60: CK505 programming failed");
        }
        identity.eeprom = match fstart_driver_lenovo::eeprom::Identity::read(smbus) {
            Ok(identity) => Some(identity),
            Err(error) => {
                fstart_log::error!(
                    "lenovo-x60: EEPROM identity unavailable, code {}",
                    error as u8
                );
                None
            }
        };
    }
    if ricoh_sd_write_protect().is_err() {
        fstart_log::error!("lenovo-x60: Ricoh SD policy programming failed");
    }
    identity
}

/// Hand EC event routing to SMM or the OS once permanent SMM is installed.
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

fn init_ck505(bus: &mut impl SmBus) -> Result<(), ServiceError> {
    use fstart_driver_intel::generic::ck505::I2cCk505;
    let mut clock = I2cCk505::new_at_address(crate::config::x60_ck505_config(), 0x69)
        .map_err(|_| ServiceError::HardwareError)?;
    clock
        .init_on_smbus(bus)
        .map_err(|_| ServiceError::HardwareError)
}

/// The onboard SD function is device 0, function 2 behind the ICH7 PCI bridge.
fn ricoh_sd_write_protect() -> Result<(), ServiceError> {
    use fstart_pci::{EcamDevice, PciType1Config};
    use tock_registers::interfaces::Readable;
    let bridge = EcamDevice::new(0, 0x1e, 0);
    // SAFETY: the enumerated ICH7 PCI bridge has a mapped Type 1 header.
    let bus = unsafe { bridge.regs::<PciType1Config>() }
        .secondary_bus
        .get();
    if bus == 0 || bus == 0xff {
        return Err(ServiceError::HardwareError);
    }
    fstart_driver_ricoh::r5c822::configure_sd_write_protect(EcamDevice::new(bus, 0, 2), true)
}

pub struct Acpi;
impl fstart_acpi::device::AcpiDevice for Acpi {
    type Config = I945Ich7AcpiContext;
    fn dsdt_aml(&self, config: &Self::Config) -> alloc::vec::Vec<u8> {
        x60_mainboard_dsdt_aml(*config)
    }
}

pub fn x60_mainboard_dsdt_aml(context: I945Ich7AcpiContext) -> alloc::vec::Vec<u8> {
    fstart_driver_lenovo::x6::acpi::dsdt_aml(&crate::config::X60_H8, context.lpc_scope())
}

static X60_SMBIOS_PROCESSOR_SOCKETS: [&str; 1] = ["Socket M"];
pub static X60_SMBIOS_IDENTITY: fstart_acpi::smbios::SmbiosIdentity<'static> =
    fstart_acpi::smbios::SmbiosIdentity {
        bios_vendor: "fstart",
        bios_version: "CBET4000 fstart 0.1.0",
        bios_release_date: fstart_platform_intel::SMBIOS_RELEASE_DATE,
        sys_manufacturer: "LENOVO",
        sys_product: "ThinkPad X60",
        sys_version: "ThinkPad X60",
        sys_serial: None,
        sys_uuid: None,
        bb_manufacturer: "LENOVO",
        bb_product: "ThinkPad X60",
        bb_version: "",
        bb_serial: None,
        chassis_type: 0x0a,
        chassis_manufacturer: "LENOVO",
        processor_sockets: &X60_SMBIOS_PROCESSOR_SOCKETS,
        oem_string: None,
    };

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_acpi::device::AcpiDevice;
    use fstart_driver_intel::i945::IntelI945;
    use fstart_driver_intel::ich7::IntelIch7;
    use fstart_driver_intel::{IntelNorthbridgeDriver, IntelSouthbridgeDriver};
    use std::fs;
    use std::process::Command;

    fn iasl(dir: &std::path::Path, args: &[&str]) {
        let output = Command::new("iasl")
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "iasl {args:?} failed: {}{}",
            std::string::String::from_utf8_lossy(&output.stdout),
            std::string::String::from_utf8_lossy(&output.stderr)
        );
    }

    /// The chipset and board fragments must form one namespace.
    #[test]
    fn complete_dsdt_iasl_round_trip() {
        let north_config = &crate::X60_PLATFORM.northbridge;
        let south_config = &crate::X60_PLATFORM.southbridge;
        let north = IntelI945::new_from_config(north_config).unwrap();
        let south = IntelIch7::new_from_config(south_config).unwrap();
        let mut aml = north.dsdt_aml(north_config);
        aml.extend(south.dsdt_aml(south_config));
        aml.extend(x60_mainboard_dsdt_aml(I945Ich7AcpiContext));
        let dsdt = fstart_acpi::platform::build_dsdt(&aml);
        let dir = std::env::temp_dir().join(std::format!("fstart-x60-dsdt-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("dsdt.aml"), &dsdt).unwrap();
        iasl(&dir, &["-d", "dsdt.aml"]);
        let source = fs::read_to_string(dir.join("dsdt.dsl")).unwrap();
        for name in [
            "Device (PEGP)",
            "Device (GFX0)",
            "Device (LCD0)",
            "Device (DOCK)",
        ] {
            assert_eq!(source.matches(name).count(), 1, "{name}");
        }
        assert!(source.contains("Name (_GPE, 0x1C)"));
        assert!(source.contains("GFX0.INCB ()"));
        assert!(source.contains("0x0008FFFF"));
        fs::rename(dir.join("dsdt.aml"), dir.join("original.aml")).unwrap();
        iasl(&dir, &["-oa", "-tc", "dsdt.dsl"]);
        fs::remove_dir_all(dir).unwrap();
    }
}
