// SPDX-License-Identifier: MIT OR Apache-2.0
//! Vilboz wiring overrides and WiFi sequencing. Board revision is runtime CBI.
use super::{dalboz, policy::Cbi};
use fstart_driver_amd::gpio::{self, Access, Delivery, Pad, Pull, Trigger};
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
pub fn configure_ram(io: &mut impl Access, cbi: Cbi, chromeos: bool) -> Result<(), gpio::Error> {
    // Must disable SPI ROM sharing BEFORE configuring GPIO67 (DMIC_SEL).
    // This function does not guess the prerequisite; the platform owns it.
    gpio::configure(io, dalboz::GPIO_SET_STAGE_RAM, overrides(cbi, chromeos))
}

/// Small board-attached I/O seam. Delay uses calibrated time, not spin counts.
pub trait BoardIo: Access {
    fn set(&mut self, pin: u8, value: bool);
    fn delay_ms(&mut self, ms: u32);
}
/// Vilboz always uses v3 WiFi reset wiring, even if EC CBI read fails.
pub fn wifi_power_reset(io: &mut impl BoardIo, cbi: Cbi) -> Result<(), gpio::Error> {
    gpio::configure(io, &[Pad::output(29, true), Pad::output(42, false)], &[])?;
    if !cbi.active_low_wifi_power() {
        io.delay_ms(10);
        io.set(42, true);
    }
    io.delay_ms(50);
    io.set(29, false);
    Ok(())
}
/// Baseboard sleep table, retained as data only: SMM/resume is not ported yet.
#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use gpio::Event;
    use std::vec::Vec;
    #[derive(Debug, PartialEq, Eq)]
    enum Step {
        Set(u8, bool),
        Delay(u32),
    }
    #[derive(Default)]
    struct Io {
        steps: Vec<Step>,
    }
    impl Access for Io {
        fn read_control(&mut self, _: u8) -> u32 {
            0
        }
        fn write_control(&mut self, _: u8, _: u32) {}
        fn mux(&mut self, _: u8, _: u8) {}
        fn master(&mut self, _: u32, _: u32) {}
        fn route(&mut self, _: u8, _: Event) {}
    }
    impl BoardIo for Io {
        fn set(&mut self, pin: u8, value: bool) {
            self.steps.push(Step::Set(pin, value));
        }
        fn delay_ms(&mut self, ms: u32) {
            self.steps.push(Step::Delay(ms));
        }
    }
    #[test]
    fn wifi_cbi_failure_is_v3_active_high_not_pre_v3() {
        for version in [None, Some(0), Some(1)] {
            let mut io = Io::default();
            wifi_power_reset(&mut io, Cbi::new(version, None)).unwrap();
            assert_eq!(
                io.steps,
                [
                    Step::Delay(10),
                    Step::Set(42, true),
                    Step::Delay(50),
                    Step::Set(29, false)
                ]
            );
        }
        let mut io = Io::default();
        wifi_power_reset(&mut io, Cbi::new(Some(2), None)).unwrap();
        assert_eq!(io.steps, [Step::Delay(50), Step::Set(29, false)]);
    }
    #[test]
    fn proximity_sensor_is_input_on_non_chromeos() {
        let table = overrides(Cbi::new(Some(2), None), false);
        assert_eq!(table[0].pin, 40);
        assert_eq!(table[0].control & (3 << 22), 0);
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
