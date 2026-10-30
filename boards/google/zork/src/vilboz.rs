// SPDX-License-Identifier: MIT OR Apache-2.0
//! Vilboz wiring overrides and WiFi sequencing. Board revision is runtime CBI.
use super::{dalboz, policy::Cbi};
use embedded_hal::{delay::DelayNs, digital::OutputPin};
use fstart_driver_amd::fch::{
    Bus,
    gpio::{self, Delivery, Gpio, Pad, Pull, Trigger},
};
const OLD: &[Pad] = &[Pad::nc(32), Pad::output(89, true), Pad::output(140, true)];
const NEW: &[Pad] = &[Pad::input(40, Pull::None), Pad::output(89, true)];
const NEW_CHROMEOS: &[Pad] = &[
    Pad::interrupt(
        40,
        Pull::None,
        Trigger::LevelLow,
        Delivery::StatusAndDelivery,
    ),
    Pad::output(89, true),
];
pub const fn overrides(cbi: Cbi, chromeos: bool) -> &'static [Pad] {
    match cbi.board_version {
        None | Some(0..=1) => OLD,
        _ if chromeos => NEW_CHROMEOS,
        _ => NEW,
    }
}
pub fn configure_ram(gpio: &Gpio<impl Bus>, cbi: Cbi, chromeos: bool) -> Result<(), gpio::Error> {
    // Must disable SPI ROM sharing BEFORE configuring GPIO67 (DMIC_SEL).
    // This function does not guess the prerequisite; the platform owns it.
    gpio.configure(dalboz::GPIO_SET_STAGE_RAM, overrides(cbi, chromeos))
}

/// WLAN reset (GPIO29, released low) and power enable (GPIO42). The PCIe
/// GPIO table leaves both to [`wifi_power_reset`]; configure these pads first.
pub const WLAN_PADS: &[Pad] = &[Pad::output(29, true), Pad::output(42, false)];
pub const WLAN_RESET_GPIO: u8 = 29;
pub const WLAN_POWER_GPIO: u8 = 42;

/// Vilboz always uses v3 WiFi reset wiring, even if EC CBI read fails.
/// `reset` and `power` are the [`WLAN_RESET_GPIO`] and [`WLAN_POWER_GPIO`]
/// outputs, configured from [`WLAN_PADS`].
pub fn wifi_power_reset<E>(
    reset: &mut impl OutputPin<Error = E>,
    power: &mut impl OutputPin<Error = E>,
    delay: &mut impl DelayNs,
    cbi: Cbi,
) -> Result<(), E> {
    if !cbi.active_low_wifi_power() {
        delay.delay_ms(10);
        power.set_high()?;
    }
    delay.delay_ms(50);
    reset.set_low()
}
/// Baseboard sleep table, retained as data only: SMM/resume is not ported yet.
#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use core::cell::RefCell;
    use embedded_hal::digital::{ErrorKind, ErrorType};
    use std::vec::Vec;
    #[derive(Debug, PartialEq, Eq)]
    enum Step {
        Set(u8, bool),
        Delay(u32),
    }
    struct Pin<'a> {
        pin: u8,
        log: &'a RefCell<Vec<Step>>,
        fail: bool,
    }
    impl ErrorType for Pin<'_> {
        type Error = ErrorKind;
    }
    impl OutputPin for Pin<'_> {
        fn set_low(&mut self) -> Result<(), ErrorKind> {
            self.set(false)
        }
        fn set_high(&mut self) -> Result<(), ErrorKind> {
            self.set(true)
        }
    }
    impl Pin<'_> {
        fn set(&mut self, value: bool) -> Result<(), ErrorKind> {
            if self.fail {
                return Err(ErrorKind::Other);
            }
            self.log.borrow_mut().push(Step::Set(self.pin, value));
            Ok(())
        }
    }
    struct Delay<'a>(&'a RefCell<Vec<Step>>);
    impl DelayNs for Delay<'_> {
        fn delay_ns(&mut self, _: u32) {}
        fn delay_ms(&mut self, ms: u32) {
            self.0.borrow_mut().push(Step::Delay(ms));
        }
    }
    fn run(cbi: Cbi, fail: bool) -> (Result<(), ErrorKind>, Vec<Step>) {
        let log = RefCell::new(Vec::new());
        let mut reset = Pin {
            pin: WLAN_RESET_GPIO,
            log: &log,
            fail,
        };
        let mut power = Pin {
            pin: WLAN_POWER_GPIO,
            log: &log,
            fail,
        };
        let result = wifi_power_reset(&mut reset, &mut power, &mut Delay(&log), cbi);
        (result, log.into_inner())
    }
    #[test]
    fn wifi_cbi_failure_is_v3_active_high_not_pre_v3() {
        for version in [None, Some(0), Some(1)] {
            let (result, steps) = run(Cbi::new(version, None), false);
            result.unwrap();
            assert_eq!(
                steps,
                [
                    Step::Delay(10),
                    Step::Set(42, true),
                    Step::Delay(50),
                    Step::Set(29, false)
                ]
            );
        }
        let (result, steps) = run(Cbi::new(Some(2), None), false);
        result.unwrap();
        assert_eq!(steps, [Step::Delay(50), Step::Set(29, false)]);
        assert_eq!(
            WLAN_PADS.iter().map(|p| p.pin).collect::<Vec<_>>(),
            [WLAN_RESET_GPIO, WLAN_POWER_GPIO]
        );
    }
    #[test]
    fn wifi_does_not_report_success_if_reset_cannot_be_released() {
        assert_eq!(run(Cbi::new(Some(4), None), true).0, Err(ErrorKind::Other));
    }
    #[test]
    fn proximity_sensor_is_input_on_non_chromeos() {
        let table = overrides(Cbi::new(Some(2), None), false);
        assert_eq!(table[0].pin, 40);
        assert_eq!(table[0].control.get() & (3 << 22), 0);
        assert_eq!(overrides(Cbi::new(None, None), false)[0].pin, 32);
    }
}

pub const SLEEP: &[Pad] = &[
    Pad::output(10, false),
    Pad::output(27, false),
    Pad::output(29, true),
    Pad::output(40, false),
    Pad::output(76, false),
];
