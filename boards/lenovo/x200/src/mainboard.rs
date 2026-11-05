//! Lenovo ThinkPad X200 mainboard work the stage `main`s call: the GPIO42
//! SMBus mux, H8/PMH7 policy, the dock connection, the identity EEPROM and
//! the X200t digitizer.

extern crate alloc;

use crate::ec;
use embedded_hal::digital::{OutputPin, PinState};
use fstart_core::services::{ServiceError, SmBus};
use fstart_driver_intel::southbridge::gpio_ich::{IchGpio, IchPin};

/// The ICH9 GPIOs the board's own devices hang off.
pub struct Pins {
    /// SMBus mux: high reaches the SPDs, low the EEPROM.
    pub smbus_mux: IchPin,
    /// Bluetooth daughter card fitted (active low).
    pub bluetooth_card: IchPin,
    /// Dock ID straps GPIO2..4; 7 means no dock.
    pub dock_id: [IchPin; 3],
    /// Connects a present dock.
    pub dock_connect: IchPin,
}

impl Pins {
    pub const fn of(gpio: IchGpio) -> Self {
        let [a, b, c] = ec::DOCK_ID_GPIOS;
        Self {
            smbus_mux: gpio.pin(crate::config::X200_SMBUS_MUX),
            bluetooth_card: gpio.pin(ec::BDC_PRESENCE_GPIO),
            dock_id: [gpio.pin(a), gpio.pin(b), gpio.pin(c)],
            dock_connect: gpio.pin(ec::DOCK_CONNECT_GPIO),
        }
    }
}

/// The branches of the X200's SMBus mux (GPIO42).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SmbusBranch {
    /// The SPDs.
    Spd,
    /// The identity EEPROM.
    Eeprom,
}

/// Switch the SMBus mux ([`Pins::smbus_mux`]).
pub fn select_smbus(mux: &mut impl OutputPin, branch: SmbusBranch) -> Result<(), ServiceError> {
    mux.set_state(PinState::from(branch == SmbusBranch::Spd))
        .map_err(|_| ServiceError::IoError)
}

/// What the board read from its own hardware for the tables.
#[derive(Default)]
pub struct Identity {
    eeprom: Option<fstart_driver_lenovo::eeprom::Identity>,
    ec_oem_string: Option<fstart_driver_lenovo::h8::EcOemString>,
}

impl Identity {
    /// The SMBIOS identity: like coreboot (`at24rf08c/lenovo_serials.c`),
    /// the EEPROM fields describe both system and board.
    pub fn smbios(&self) -> fstart_acpi::smbios::SmbiosIdentity<'_> {
        let mut identity = X200_SMBIOS_IDENTITY;
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
/// The EC options this ThinkPad has, as its coreboot `cmos.layout` lists
/// them.
pub static EC_KNOBS: [fstart_options::Knob<fstart_driver_lenovo::options::Options>; 11] = {
    use fstart_driver_lenovo::options::*;
    [
        BLUETOOTH,
        WWAN,
        WLAN,
        UWB,
        TRACKPOINT,
        FN_CTRL_SWAP,
        STICKY_FN,
        POWER_MANAGEMENT_BEEPS,
        LOW_BATTERY_BEEP,
        VOLUME,
        FIRST_BATTERY,
    ]
};

/// EC/PMH7, the dock, the identity EEPROM and the digitizer, once the
/// chipset devices are up. Returns the identity and the board ACPI device.
pub fn init_devices(
    pins: &mut Pins,
    smbus: &mut impl SmBus,
    ec: &fstart_driver_lenovo::options::Options,
    resume: bool,
) -> (Identity, Acpi) {
    let mut identity = Identity::default();
    let (bluetooth, acknowledged) = ec::init(
        &mut pins.bluetooth_card,
        &mut pins.dock_id,
        &mut pins.dock_connect,
        &crate::config::X200_H8,
        ec,
        resume,
    );
    if !acknowledged {
        fstart_log::error!("lenovo-x200: H8 EC initialization incomplete");
    }
    if select_smbus(&mut pins.smbus_mux, SmbusBranch::Eeprom).is_err() {
        fstart_log::error!("lenovo-x200: SMBus routing to the EEPROM failed");
    }
    if !resume {
        // Read-only identity access never touches the EEPROM's data or
        // protection registers.
        identity.eeprom = fstart_driver_lenovo::eeprom::Identity::read(smbus)
            .inspect_err(|error| {
                fstart_log::error!(
                    "lenovo-x200: EEPROM identity unavailable, code {}",
                    *error as u8
                )
            })
            .ok();
        identity.ec_oem_string = fstart_driver_lenovo::h8::H8::new(
            fstart_driver_lenovo::ec::Ec::new(crate::config::X200_H8.resources.os),
        )
        .smbios_oem_string();
        if identity.ec_oem_string.is_none() {
            fstart_log::error!("lenovo-x200: H8 EC firmware id unavailable");
        }
    }
    // The digitizer's SuperIO is programmed on every boot; whether it is a
    // tablet only matters for the tables a cold boot builds.
    let digitizer = ec::digitizer_init(identity.eeprom.as_ref().map(|e| e.part_number()));
    (
        identity,
        Acpi {
            bluetooth,
            digitizer,
        },
    )
}

/// The board's ACPI: H8 EC surface, sleep hooks, ThinkLight, dock and the
/// X200t digitizer from the Lenovo driver.
pub struct Acpi {
    bluetooth: bool,
    digitizer: bool,
}

impl fstart_acpi::device::AcpiDevice for Acpi {
    type Config = ();

    fn dsdt_aml(&self, _: &Self::Config) -> alloc::vec::Vec<u8> {
        x200_mainboard_dsdt_aml(self.bluetooth, self.digitizer)
    }
}

/// Assemble the X200 DSDT.
pub fn x200_mainboard_dsdt_aml(bluetooth: bool, digitizer: bool) -> alloc::vec::Vec<u8> {
    let h8 = fstart_driver_lenovo::h8::H8Config {
        has_bluetooth: bluetooth,
        ..crate::config::X200_H8
    };
    crate::acpi::dsdt_aml::<<crate::Board as fstart_platform_intel::IntelBoardFacts>::Platform>(
        &h8,
        fstart_driver_intel::ich9::ich9::DEFAULT_GPIOBASE,
        digitizer,
    )
}

static X200_SMBIOS_PROCESSOR_SOCKETS: [&str; 1] = ["BGA956"];

pub static X200_SMBIOS_IDENTITY: fstart_acpi::smbios::SmbiosIdentity<'static> =
    fstart_acpi::smbios::SmbiosIdentity {
        bios_vendor: "fstart",
        // Non-OEM firmware ID, following coreboot and the X61 binding;
        // Linux thinkpad_acpi validates this before the ThinkPad model string.
        bios_version: "CBET4000 fstart 0.1.0",
        bios_release_date: fstart_platform_intel::SMBIOS_RELEASE_DATE,
        sys_manufacturer: "LENOVO",
        sys_product: "ThinkPad X200",
        sys_version: "ThinkPad X200",
        sys_serial: None,
        sys_uuid: None,
        bb_manufacturer: "LENOVO",
        bb_product: "ThinkPad X200",
        bb_version: "1.0",
        bb_serial: None,
        chassis_type: 0x0a,
        chassis_manufacturer: "LENOVO",
        processor_sockets: &X200_SMBIOS_PROCESSOR_SOCKETS,
        oem_string: None,
    };

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn smbus_mux_reaches_the_spds_high_and_the_eeprom_low() {
        let mut mux = Levels::default();
        select_smbus(&mut mux, SmbusBranch::Spd).unwrap();
        select_smbus(&mut mux, SmbusBranch::Eeprom).unwrap();
        assert_eq!(mux.0, [true, false]);
    }
}
