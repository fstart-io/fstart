//! T6 board work. Reusable drivers own register encodings; the board owns
//! dock/EC sequencing. No S3, ATI initialization/brightness, TI CardBus vendor
//! setup, IR, parallel-port or thermal-safety support is claimed.

use embedded_hal::digital::InputPin;
use fstart_core::services::{ServiceError, SmBus};
use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};

pub const fn console() -> Ns16550Config {
    Ns16550Config {
        regs: AccessMode::Pio {
            base: crate::UART0_PIO_BASE as u64,
        },
        clock_freq: crate::UART0_CLOCK_FREQ,
        baud_rate: crate::UART0_BAUD_RATE,
    }
}

pub fn setup_console(legacy_module: &mut impl InputPin) -> bool {
    crate::dock::setup_console(legacy_module, &crate::T6_H8).is_err()
}

fn os_ec() -> fstart_driver_lenovo::ec::Ec {
    fstart_driver_lenovo::ec::Ec::new(crate::T6_H8.resources.os)
}

pub struct Devices {
    pub ide_primary: Option<bool>,
    pub native_brightness: bool,
    /// The Bluetooth card is fitted.
    pub bluetooth: bool,
}
/// The EC options this ThinkPad has, as its coreboot `cmos.layout` lists
/// them.
pub static EC_KNOBS: [fstart_options::Knob<fstart_driver_lenovo::options::Options>; 10] = {
    use fstart_driver_lenovo::options::*;
    [
        TOUCHPAD,
        TRACKPOINT,
        BLUETOOTH,
        WWAN,
        WLAN,
        STICKY_FN,
        POWER_MANAGEMENT_BEEPS,
        LOW_BATTERY_BEEP,
        VOLUME,
        FIRST_BATTERY,
    ]
};

pub fn init_ec_and_bay(
    bdc: &mut impl InputPin,
    legacy_module: &mut impl InputPin,
    console_failed: bool,
    ec: &fstart_driver_lenovo::options::Options,
) -> Result<Devices, ServiceError> {
    use crate::dock;
    use fstart_driver_lenovo::{h8::H8, pmh7::Pmh7};
    let bluetooth = crate::T6_H8.bdc_installed(bdc);
    Pmh7::new(crate::T6_H8.resources.pmh7_base.raw()).enable(&crate::T6_PMH7, ec);
    // No S3: always a cold EC configuration.
    if !H8::new(os_ec()).enable(&crate::T6_H8, bluetooth, ec, false) {
        fstart_log::error!("T6: EC/thermal initialization incomplete");
    }
    let legacy = dock::legacy_io_present(legacy_module)?;
    let bay = H8::new(os_ec()).ultrabay_device_present();
    let ide_primary = if let Some(present) = bay {
        Some(dock::ide_primary_enabled(legacy, present))
    } else {
        fstart_log::error!("T6: Ultrabay presence unavailable; PATA remains disabled");
        None
    };
    if !dock::mainstage_power_policy(os_ec(), &crate::T6_H8, legacy) {
        fstart_log::error!("T6: Ultrabay power/LED policy incomplete");
    }
    let native_brightness = fstart_pci::EcamDevice::new(0, 2, 0).vendor_id() == 0x8086;
    fstart_log::info!(
        "T6: experimental, no hardware/thermal validation; S3, ATI, TI CardBus setup, IR and parallel unsupported"
    );
    if console_failed {
        fstart_log::error!("T6: dock/module setup failed; serial console may be unavailable");
    }
    Ok(Devices {
        ide_primary,
        native_brightness,
        bluetooth,
    })
}

pub struct Identity {
    pub eeprom: Option<fstart_driver_lenovo::eeprom::Identity>,
    pub ec_oem: Option<fstart_driver_lenovo::h8::EcOemString>,
}

pub fn init_clock_and_identity(smbus: &mut impl SmBus) -> Result<Identity, ServiceError> {
    use fstart_driver_intel::generic::ck505::I2cCk505;
    let mut clock = I2cCk505::new_at_address(crate::ck505_config(), 0x69)?;
    if clock.init_on_smbus(smbus).is_err() {
        fstart_log::error!("T6: CK505 initialization failed");
    }
    let eeprom = fstart_driver_lenovo::eeprom::Identity::read(smbus).ok();
    let ec_oem = fstart_driver_lenovo::h8::H8::new(os_ec()).smbios_oem_string();
    Ok(Identity { eeprom, ec_oem })
}

pub fn handoff_ec_events() {
    use fstart_driver_intel::southbridge::smi::{APM_CNT, APM_CNT_ACPI_DISABLE};
    // SAFETY: permanent board SMM has been installed and owns APMC.
    unsafe { fstart_core::pio::outb(APM_CNT, APM_CNT_ACPI_DISABLE) };
}

pub struct Acpi {
    pub native_brightness: bool,
    /// The Bluetooth card is fitted.
    pub bluetooth: bool,
}
