//! Lenovo ThinkPad X60 mainboard work called by the stage mains.

use crate::dock;
use embedded_hal::digital::InputPin;
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
            dock_present: gpio.pin(crate::config::DOCK_PRESENT_GPIO),
            bluetooth_card: gpio.pin(crate::config::BDC_PRESENCE_GPIO),
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

/// Initialize EC, dock, clock, EEPROM and SD policy in their original order.
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
    let config = &crate::config::X60_H8;
    let bluetooth = config.bdc_installed(bluetooth_card);
    Pmh7::new(config.resources.pmh7_base.raw()).enable(&crate::config::X60_PMH7, options);
    let initialized = H8::new(os_ec()).enable(config, bluetooth, options, resume);
    let latched = dock::latch_connected_dock(os_ec());
    if !(initialized && latched) {
        fstart_log::error!("lenovo-x60: H8 EC initialization incomplete");
    }
    bluetooth
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

/// The board's ACPI: sleep/wake hooks, the UltraBase dock, the EC wake GPE
/// and the H8 EC surface.
pub struct Acpi {
    /// The Bluetooth card is fitted.
    pub bluetooth: bool,
}
