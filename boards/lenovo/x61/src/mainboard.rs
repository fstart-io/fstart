//! Lenovo ThinkPad X61 mainboard work the stage `main`s call.
//!
//! This module holds the board-specific parts that do not belong in the
//! reusable GM965 northbridge or ICH8 southbridge drivers. The important
//! pre-console path is the X6 UltraBase dock: ICH8 opens the LPC/GPIO decode
//! windows, then [`setup_dock_console`] initializes the laptop-side DLPC,
//! connects the dock-side LPC bus when present, and enables the dock PC87392
//! COM1 before the NS16550 console driver probes port 0x3f8.

#![allow(clippy::result_unit_err)]

use crate::dock;
use embedded_hal::digital::{InputPin, OutputPin, PinState};
use fstart_core::services::{ServiceError, SmBus};
use fstart_driver_intel::southbridge::gpio_ich::{IchGpio, IchPin};
use fstart_driver_lenovo::h8::H8;
use fstart_driver_lenovo::pmh7::Pmh7;
use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};

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
            dock_present: gpio.pin(crate::config::DOCK_PRESENT_GPIO),
            bluetooth_card: gpio.pin(crate::config::BDC_PRESENCE_GPIO),
            smbus_mux: gpio.pin(crate::config::X61_SMBUS_MUX.pin),
        }
    }
}

/// Route the console through the dock before the UART is probed.
pub fn setup_dock_console(dock_present: &mut impl InputPin) {
    dock::setup_console(dock_present, os_ec());
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
    dock::dock_present(dock_present) && dock::ultrabay_present()
}

/// What the board read from its own hardware for the tables.
#[derive(Default)]
pub struct Identity {
    pub eeprom: Option<fstart_driver_lenovo::eeprom::Identity>,
    pub ec_oem_string: Option<fstart_driver_lenovo::h8::EcOemString>,
}
/// The EC options this ThinkPad has, as its coreboot `cmos.layout` lists
/// them.
pub static EC_KNOBS: [fstart_options::Knob<fstart_driver_lenovo::options::Options>; 9] = {
    use fstart_driver_lenovo::options::*;
    [
        BLUETOOTH,
        WWAN,
        WLAN,
        TRACKPOINT,
        STICKY_FN,
        POWER_MANAGEMENT_BEEPS,
        LOW_BATTERY_BEEP,
        VOLUME,
        FIRST_BATTERY,
    ]
};

/// EC/PMH7, dock power, the identity EEPROM and the SD slot, once the
/// chipset devices are up. `smbus` is on the EEPROM branch. EC setup is
/// independent of ACPI table emission. Returns what the tables need on a
/// cold boot.
pub fn init_devices(
    bluetooth_card: &mut impl InputPin,
    smbus: &mut impl SmBus,
    ec: &fstart_driver_lenovo::options::Options,
    resume: bool,
) -> (Identity, Acpi) {
    let mut identity = Identity::default();
    let bluetooth = init_ec(bluetooth_card, ec, resume);
    if !resume {
        identity.ec_oem_string = H8::new(os_ec()).smbios_oem_string();
        if identity.ec_oem_string.is_none() {
            fstart_log::error!("lenovo-x61: H8 EC firmware id unavailable");
        }
    }
    dock::mainstage_power_policy(os_ec());
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
    (identity, Acpi { bluetooth })
}

/// coreboot `h8_enable()` and the PMH7 `enable_dev()` with the devicetree's
/// registers, then the EC's latch for a dock the stages connected. Returns
/// whether the Bluetooth card is fitted.
fn init_ec(
    bluetooth_card: &mut impl InputPin,
    options: &fstart_driver_lenovo::options::Options,
    resume: bool,
) -> bool {
    let config = &crate::config::X61_H8;
    let bluetooth = config.bdc_installed(bluetooth_card);
    Pmh7::new(config.resources.pmh7_base.raw()).enable(&crate::config::X61_PMH7, options);
    let initialized = H8::new(os_ec()).enable(config, bluetooth, options, resume);
    let latched = dock::latch_connected_dock(os_ec());
    if !(initialized && latched) {
        fstart_log::error!("lenovo-x61: H8 EC initialization incomplete");
    }
    bluetooth
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

/// The board's ACPI: sleep/wake hooks, the UltraBase dock, the EC wake GPE
/// and the H8 EC surface.
pub struct Acpi {
    /// The Bluetooth card is fitted.
    pub bluetooth: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
