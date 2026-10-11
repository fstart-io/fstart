//! Verified W83627DHG/EHG GPIO1/3/4/5 indexed-register machinery.
//!
//! GPIO1 is verified only for EHG. GPIO3/4/5 share the register layout in
//! DHG V1.4 §20.10 and EHG V1.3 §§7.8/7.10. Preserve unrequested controls;
//! bank-wide pin muxes still affect every pad in their bank.
use crate::{SuperIo, SuperIoChip};
use tock_registers::{
    fields::{Field, FieldValue},
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

register_bitfields![u8,
    PIN_BITS [ PINS OFFSET(0) NUMBITS(8) [] ],
    MULTIFUNCTION [
        GP34_RESET OFFSET(7) NUMBITS(1) [], GP33_RESET OFFSET(6) NUMBITS(1) [],
        GP32_RESET OFFSET(5) NUMBITS(1) [],
        UART_B_PINS OFFSET(0) NUMBITS(2) [Gpio = 2],
    ],
    SMBUS [ ENABLE OFFSET(1) NUMBITS(1) [] ],
    GPIO1_MUX [ GPIO OFFSET(0) NUMBITS(1) [] ],
];

#[derive(Clone, Copy)]
pub(crate) enum Profile {
    Dhg,
    Ehg,
}

/// Requested logical value. Selected inversion also inverts the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpioDirection {
    Input,
    Output(bool),
}

/// GP32–34, GP40–47 and GP50–57 are verified on both chips; GP10–17
/// are EHG-only. Chip-specific validation happens before configuration I/O.
/// GPIO4 shares a bank-wide mux with UART B and cannot coexist with COM2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpioPinConfig {
    pin: u8,
    direction: GpioDirection,
    inverted: bool,
}
impl GpioPinConfig {
    #[must_use]
    pub const fn input(pin: u8) -> Self {
        assert!(matches!(pin, 10..=17 | 32..=34 | 40..=47 | 50..=57));
        Self {
            pin,
            direction: GpioDirection::Input,
            inverted: false,
        }
    }
    #[must_use]
    pub const fn output(pin: u8, high: bool) -> Self {
        Self {
            direction: GpioDirection::Output(high),
            ..Self::input(pin)
        }
    }
    #[must_use]
    pub const fn inverted(mut self) -> Self {
        self.inverted = true;
        self
    }

    fn bank(self) -> (u8, u8) {
        match self.pin / 10 {
            1 => (7, 0),
            group @ 3..=5 => (9, group - 2),
            _ => unreachable!(),
        }
    }
    fn settings(self) -> impl Iterator<Item = (u8, FieldValue<u8, PIN_BITS::Register>)> {
        let direction = match self.pin / 10 {
            1 | 3 => 0xf0,
            4 => 0xf4,
            5 => 0xe0,
            _ => unreachable!(),
        };
        let field = Field::<u8, PIN_BITS::Register>::new(1, (self.pin % 10) as usize);
        [
            // Data writes to inputs are ignored: direction must precede data.
            Some((
                direction,
                field.val(matches!(self.direction, GpioDirection::Input) as u8),
            )),
            Some((direction + 2, field.val(self.inverted as u8))),
            match self.direction {
                GpioDirection::Output(high) => Some((direction + 1, field.val(high as u8))),
                GpioDirection::Input => None,
            },
            // Per-pin watchdog/LED overrides accompany the bank-wide mux.
            (self.pin / 10 == 1).then_some((0xf3, field.val(0))),
            (self.pin / 10 == 4).then_some((0xf7, field.val(0))),
            (self.pin / 10 == 5).then_some((0x2d, field.val(1))),
        ]
        .into_iter()
        .flatten()
    }
}

/// Separate strap-affecting data/mux updates from ordinary initialization.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GpioUpdate {
    pub output_or_routing_changed: bool,
    pub direction_or_activation_changed: bool,
}
impl GpioUpdate {
    fn record_setting(&mut self, index: u8, changed: bool) {
        if matches!(index, 0xf0 | 0xf4 | 0xe0) {
            self.direction_or_activation_changed |= changed;
        } else {
            self.output_or_routing_changed |= changed;
        }
    }
}

fn valid(pins: &[GpioPinConfig], profile: Profile, com2: bool) -> bool {
    let unique = !pins
        .iter()
        .enumerate()
        .any(|(i, pin)| pins[i + 1..].iter().any(|other| pin.pin == other.pin));
    let gpio1 = pins.iter().filter(|pin| pin.pin / 10 == 1).count();
    unique
        && (gpio1 == 0 || (matches!(profile, Profile::Ehg) && gpio1 == 8))
        && (!com2 || !pins.iter().any(|pin| pin.pin / 10 == 4))
}

// Private to the two chip drivers: no arbitrary Super I/O can opt into this
// verified profile through the public API.
pub(crate) fn configure<C: SuperIoChip>(
    sio: &mut SuperIo<C>,
    pins: &[GpioPinConfig],
    profile: Profile,
) -> GpioUpdate {
    assert!(valid(pins, profile, sio.config.com2.is_some()));
    if pins.is_empty() {
        return GpioUpdate::default();
    }
    let gpio1 = pins.iter().any(|pin| pin.pin / 10 == 1);
    let gpio4 = pins.iter().any(|pin| pin.pin / 10 == 4);
    sio.enter_config();
    if gpio4 {
        sio.logical_device(3).set_enabled(false);
    }
    // GPIO1 replaces the whole game-port bank. Require all eight pads above,
    // disable the game function, then prepare each control before switching.
    if gpio1 {
        sio.logical_device(7).set_activation_bit(1, false);
    }
    let mut changed = GpioUpdate::default();
    for pin in pins {
        sio.logical_device(pin.bank().0);
        for (index, fields) in pin.settings() {
            let register = sio.register::<PIN_BITS::Register>(index);
            let old = register.get();
            let new = fields.modify(old);
            changed.record_setting(index, new != old);
            register.set(new);
        }
    }
    if pins.iter().any(|pin| matches!(pin.pin, 32 | 33)) {
        let register = sio.register::<SMBUS::Register>(0x2a);
        changed.output_or_routing_changed |= register.is_set(SMBUS::ENABLE);
        register.modify(SMBUS::ENABLE::CLEAR);
    }
    let mux = pins
        .iter()
        .filter_map(|pin| match pin.pin {
            32 => Some(MULTIFUNCTION::GP32_RESET::CLEAR),
            33 => Some(MULTIFUNCTION::GP33_RESET::CLEAR),
            34 => Some(MULTIFUNCTION::GP34_RESET::CLEAR),
            _ => None,
        })
        .reduce(|fields, field| fields + field);
    let mux = if gpio4 {
        Some(mux.map_or(MULTIFUNCTION::UART_B_PINS::Gpio, |fields| {
            fields + MULTIFUNCTION::UART_B_PINS::Gpio
        }))
    } else {
        mux
    };
    if let Some(fields) = mux {
        let register = sio.register::<MULTIFUNCTION::Register>(0x2c);
        let old = register.get();
        changed.output_or_routing_changed |= fields.modify(old) != old;
        register.modify(fields);
    }
    if gpio1 {
        let register = sio.register::<GPIO1_MUX::Register>(0x29);
        changed.output_or_routing_changed |= !register.is_set(GPIO1_MUX::GPIO);
        register.modify(GPIO1_MUX::GPIO::SET);
    }
    // Select the LDN explicitly: GPIO1 and GPIO3 use the same register indices.
    for group in [1, 3, 4, 5] {
        if let Some(pin) = pins.iter().find(|pin| pin.pin / 10 == group) {
            let (ldn, bit) = pin.bank();
            sio.logical_device(ldn);
            let field = Field::<u8, PIN_BITS::Register>::new(1, bit as usize);
            changed.direction_or_activation_changed |=
                sio.register::<PIN_BITS::Register>(0x30).read(field) == 0;
            sio.logical_device(ldn).set_activation_bit(bit, true);
        }
    }
    sio.exit_config();
    changed
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    #[test]
    fn direction_initialization_alone_is_not_a_strap_reset_request() {
        let mut changes = GpioUpdate::default();
        changes.record_setting(0xf0, true);
        changes.record_setting(0xe0, true);
        assert!(changes.direction_or_activation_changed);
        assert!(!changes.output_or_routing_changed);
        changes.record_setting(0xf2, true);
        assert!(changes.output_or_routing_changed);
        let mut data = GpioUpdate::default();
        data.record_setting(0xe1, true);
        assert!(data.output_or_routing_changed);
        assert!(!data.direction_or_activation_changed);
    }

    #[test]
    fn bsel_pin_programming_obeys_direction_before_data_and_preserves_neighbors() {
        for pin in [32, 33, 55] {
            for high in [false, true] {
                let settings: Vec<_> = GpioPinConfig::output(pin, high)
                    .inverted()
                    .settings()
                    .collect();
                let mask = 1 << (pin % 10);
                assert_eq!(settings[1].0, settings[0].0 + 2);
                assert_eq!(settings[2].0, settings[0].0 + 1);
                assert_eq!(settings[0].1.modify(0xff), !mask);
                assert_eq!(settings[1].1.modify(0xa5), 0xa5 | mask);
                assert_eq!(
                    settings[2].1.modify(0xa5),
                    (0xa5 & !mask) | if high { mask } else { 0 }
                );
            }
        }
    }

    #[test]
    fn input_does_not_write_ignored_data_and_gpio5_mux_is_per_pin() {
        let settings: Vec<_> = GpioPinConfig::input(55).settings().collect();
        assert_eq!(
            settings.iter().map(|s| s.0).collect::<Vec<_>>(),
            [0xe0, 0xe2, 0x2d]
        );
        assert_eq!(settings[2].1.modify(1), 0x21);
        let settings: Vec<_> = GpioPinConfig::output(42, false).settings().collect();
        assert_eq!(settings.last().unwrap().0, 0xf7);
        assert_eq!(settings.last().unwrap().1.modify(0xff), 0xfb);
    }

    #[test]
    fn mux_changes_never_treat_read_only_or_reserved_bit_as_policy() {
        let fields = MULTIFUNCTION::GP32_RESET::CLEAR
            + MULTIFUNCTION::GP33_RESET::CLEAR
            + MULTIFUNCTION::UART_B_PINS::Gpio;
        for old in [0xe2, 0xf2] {
            let new = fields.modify(old);
            assert_eq!(new & 0x10, old & 0x10);
            assert_eq!(fields.modify(new), new);
        }
    }

    #[test]
    fn ehg_gpio1_requires_complete_bank_and_is_not_a_dhg_capability() {
        let pins = core::array::from_fn::<_, 8, _>(|i| GpioPinConfig::input(10 + i as u8));
        assert!(valid(&pins, Profile::Ehg, true));
        assert!(!valid(&pins[..7], Profile::Ehg, false));
        assert!(!valid(&pins, Profile::Dhg, false));
        assert!(!valid(&[GpioPinConfig::input(42)], Profile::Ehg, true));
        assert!(!valid(&[pins[0], pins[0]], Profile::Ehg, false));
        assert_eq!(pins[0].bank(), (7, 0));
        assert_eq!(GpioPinConfig::input(32).bank(), (9, 1));
        assert_eq!(GpioPinConfig::input(55).bank(), (9, 3));
        let settings: Vec<_> = pins[6].settings().collect();
        assert_eq!(
            settings.iter().map(|s| s.0).collect::<Vec<_>>(),
            [0xf0, 0xf2, 0xf3]
        );
        assert_eq!(settings.last().unwrap().1.modify(0xff), 0xbf);
    }

    #[test]
    #[should_panic]
    fn reserved_or_unverified_pin_rejected() {
        let _ = GpioPinConfig::input(38);
    }
}
