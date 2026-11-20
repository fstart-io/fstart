//! Lenovo ThinkPad T400-family mainboard work the stage `main`s call:
//! dock serial bring-up, hybrid/discrete graphics policy, the GPIO42 SMBus
//! mux, H8/PMH7 policy, and the identity EEPROM.

use crate::ec;
use embedded_hal::digital::{InputPin, OutputPin, PinState};
use fstart_core::services::{ServiceError, SmBus};
use fstart_driver_intel::gm45::Gm45GraphicsPolicy;
use fstart_driver_intel::southbridge::gpio_ich::{IchGpio, IchPin};
use fstart_driver_lenovo::hybrid_graphics::{self, HybridGraphicsMode, HybridGraphicsOptions};
use fstart_driver_lenovo::t6::dock;
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

/// The ICH9 GPIOs the board's own devices hang off.
pub struct Pins {
    /// SMBus mux: high reaches the SPDs, low the EEPROM.
    pub smbus_mux: IchPin,
    /// Bluetooth daughter card fitted (active low, non-R500).
    pub bluetooth_card: IchPin,
    /// Discrete GPU presence strap (low = dGPU installed).
    pub dgpu_detect: IchPin,
    /// Hybrid graphics internal panel mux (low = integrated).
    pub panel_mux: IchPin,
    /// Hybrid graphics backlight mux (low = integrated).
    pub backlight_mux: IchPin,
    /// Discrete GPU power enable (low = off, high = on).
    pub dgpu_power: IchPin,
}

impl Pins {
    pub const fn of(gpio: IchGpio) -> Self {
        Self {
            smbus_mux: gpio.pin(crate::config::T400_SMBUS_MUX),
            bluetooth_card: gpio.pin(ec::BDC_PRESENCE_GPIO),
            dgpu_detect: gpio.pin(crate::config::DGPU_DETECT_GPIO),
            panel_mux: gpio.pin(crate::config::PANEL_MUX_GPIO),
            backlight_mux: gpio.pin(crate::config::BACKLIGHT_MUX_GPIO),
            dgpu_power: gpio.pin(crate::config::DGPU_POWER_GPIO),
        }
    }
}

/// The branches of the T400's SMBus mux (GPIO42).
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

/// Connect the Mini Dock PC87382/PC87384 serial port if a serial-capable dock
/// is attached (coreboot `t400/bootblock.c` + `t400/dock.c`).
pub fn setup_dock_console() -> Result<dock::ConsoleRoute, fstart_core::services::device::DeviceError>
{
    dock::setup_t400_dock_console(&crate::config::T400_H8)
}

/// Early bootblock graphics selection (coreboot `t400/romstage.c`):
/// - R500: GPIO21 high = integrated, low = discrete.
/// - T400/T500/R400/W500: Lenovo hybrid graphics driver using `mode` and
///   dGPU power GPIO49.
pub fn select_early_graphics(
    detect: &mut impl InputPin,
    dgpu_power: &mut impl OutputPin,
    mode: HybridGraphicsMode,
) -> Result<Gm45GraphicsPolicy, ServiceError> {
    if crate::config::IS_R500 {
        return detect
            .is_low()
            .map(|discrete| {
                if discrete {
                    Gm45GraphicsPolicy::Discrete
                } else {
                    Gm45GraphicsPolicy::Integrated
                }
            })
            .map_err(|_| ServiceError::IoError);
    }
    let sel = hybrid_graphics::early_select(detect, dgpu_power, false, mode);
    Ok(match (sel.igd, sel.peg) {
        (true, true) => Gm45GraphicsPolicy::Hybrid,
        (false, true) => Gm45GraphicsPolicy::Discrete,
        _ => Gm45GraphicsPolicy::Integrated,
    })
}

/// What the board read from its own hardware for the tables.
#[derive(Default)]
pub struct Identity {
    pub eeprom: Option<fstart_driver_lenovo::eeprom::Identity>,
    pub ec_oem_string: Option<fstart_driver_lenovo::h8::EcOemString>,
}

/// The EC options this ThinkPad has, as its coreboot `cmos.layout` lists them.
pub static EC: fstart_options::OptionSet<fstart_driver_lenovo::options::Options> = {
    use fstart_driver_lenovo::options::*;
    fstart_options::OptionSet {
        knobs: &[
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
        ],
        board: Options::DEFAULT,
    }
};

/// Hybrid graphics option (`hybrid_graphics_mode`), exposed on T400/T500/R400/W500.
pub static HYBRID_GRAPHICS: fstart_options::OptionSet<HybridGraphicsOptions> =
    fstart_options::OptionSet {
        knobs: &[hybrid_graphics::HYBRID_GRAPHICS_MODE],
        board: HybridGraphicsOptions::DEFAULT,
    };

/// EC/PMH7, hybrid graphics panel mux, and the identity EEPROM, once the
/// chipset devices are up.
pub fn init_devices(
    pins: &mut Pins,
    smbus: &mut impl SmBus,
    ec_opts: &fstart_driver_lenovo::options::Options,
    graphics: Gm45GraphicsPolicy,
    resume: bool,
) -> (Identity, Acpi) {
    let mut identity = Identity::default();
    if !crate::config::IS_R500 {
        // Use the live DEVEN policy, not a second evaluation of CMOS.
        hybrid_graphics::mux_panel(
            &mut pins.dgpu_detect,
            &mut pins.panel_mux,
            false,
            &mut pins.backlight_mux,
            false,
            graphics == Gm45GraphicsPolicy::Discrete,
        );
    }
    let (bluetooth, acknowledged) = ec::init(
        &mut pins.bluetooth_card,
        &crate::config::T400_H8,
        ec_opts,
        resume,
    );
    if !acknowledged {
        fstart_log::error!("lenovo-t400: H8 EC initialization incomplete");
    }
    if select_smbus(&mut pins.smbus_mux, SmbusBranch::Eeprom).is_err() {
        fstart_log::error!("lenovo-t400: SMBus routing to the EEPROM failed");
    }
    if !resume {
        identity.eeprom = fstart_driver_lenovo::eeprom::Identity::read(smbus)
            .inspect_err(|error| {
                fstart_log::error!(
                    "lenovo-t400: EEPROM identity unavailable, code {}",
                    *error as u8
                )
            })
            .ok();
        identity.ec_oem_string = fstart_driver_lenovo::h8::H8::new(
            fstart_driver_lenovo::ec::Ec::new(crate::config::T400_H8.resources.os),
        )
        .smbios_oem_string();
        if identity.ec_oem_string.is_none() {
            fstart_log::error!("lenovo-t400: H8 EC firmware id unavailable");
        }
    }
    (identity, Acpi { bluetooth })
}

/// The board's ACPI: H8 EC surface, sleep hooks, ThinkLight, and dock.
pub struct Acpi {
    pub bluetooth: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

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
