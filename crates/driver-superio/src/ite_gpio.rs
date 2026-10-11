//! Shared IT8718F/IT8720F GPIO register layout, with chip-specific mux limits.
//! See IT8718F V0.3 §§8.3, 8.11 and IT8720F V0.1 §§8.3, 8.11.
use crate::{IoResource, SuperIo, SuperIoChip};
use core::marker::PhantomData;
use tock_registers::{interfaces::ReadWriteable, register_bitfields};

register_bitfields![u8, GPIO_PINS [ PINS OFFSET(0) NUMBITS(8) [] ]];

/// Chip differences within this verified GPIO register layout.
pub trait IteGpioChip: SuperIoChip + Copy + core::fmt::Debug {
    const MAX_GPIO: u8;
    const HAS_GP15: bool;
    const EXTENDED_GPIO1: bool;
    /// Pads without internal pull-ups, beyond the common group-2 exclusion.
    const PULL_UP_EXCLUSIONS: &'static [u8] = &[];
}

const COUPLED_GROUPS: [&[u8]; 2] = [&[56, 57, 60, 61, 62], &[63, 64, 65, 66, 67]];

/// `Alternate` is the GPIO block's special-function routing, not the native peripheral.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpioFunction {
    Peripheral,
    Alternate,
    SimpleIo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpioDirection {
    Input,
    /// Retain the existing latch; initialization is not glitch-free switching.
    Output,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullUp {
    Disabled,
    Enabled,
}

/// Physical GPxx pin policy. Omitted settings and pins are preserved.
#[derive(Debug, Clone, Copy)]
pub struct GpioPinConfig<C: IteGpioChip> {
    pin: u8,
    function: GpioFunction,
    direction: Option<GpioDirection>,
    pull_up: Option<PullUp>,
    inverted: Option<bool>,
    chip: PhantomData<C>,
}

impl<C: IteGpioChip> GpioPinConfig<C> {
    #[must_use]
    pub const fn new(pin: u8, function: GpioFunction) -> Self {
        assert!(pin >= 10 && pin <= C::MAX_GPIO && pin % 10 < 8 && pin != 45);
        assert!(pin != 15 || C::HAS_GP15);
        // IT8718F set 6 supports simple I/O only (§5, serial-port-2 pads).
        assert!(pin < 60 || !matches!(function, GpioFunction::Alternate));
        Self {
            pin,
            function,
            direction: None,
            pull_up: None,
            inverted: None,
            chip: PhantomData,
        }
    }

    #[must_use]
    pub const fn peripheral(pin: u8) -> Self {
        Self::new(pin, GpioFunction::Peripheral)
    }

    #[must_use]
    pub const fn simple_io(pin: u8) -> Self {
        Self::new(pin, GpioFunction::SimpleIo)
    }

    #[must_use]
    pub const fn input(pin: u8) -> Self {
        Self::simple_io(pin).direction(GpioDirection::Input)
    }

    #[must_use]
    pub const fn direction(mut self, direction: GpioDirection) -> Self {
        self.direction = Some(direction);
        self
    }

    #[must_use]
    pub const fn pull_up(mut self, pull_up: PullUp) -> Self {
        assert!(self.pin / 10 != 2);
        let mut index = 0;
        while index < C::PULL_UP_EXCLUSIONS.len() {
            assert!(self.pin != C::PULL_UP_EXCLUSIONS[index]);
            index += 1;
        }
        self.pull_up = Some(pull_up);
        self
    }

    #[must_use]
    pub const fn inverted(mut self, inverted: bool) -> Self {
        self.inverted = Some(inverted);
        self
    }

    fn gpio_selected(self) -> bool {
        self.function != GpioFunction::Peripheral
    }

    fn mux_bit(self) -> u8 {
        match self.pin {
            56 | 57 | 60..=62 => 6,
            63..=67 => 7,
            _ => self.pin % 10,
        }
    }

    fn settings(
        self,
    ) -> impl Iterator<
        Item = (
            u8,
            tock_registers::fields::FieldValue<u8, GPIO_PINS::Register>,
        ),
    > {
        let group = self.pin / 10 - 1;
        let field = tock_registers::fields::Field::<u8, GPIO_PINS::Register>::new(
            1,
            (self.pin % 10) as usize,
        );
        let mux = tock_registers::fields::Field::<u8, GPIO_PINS::Register>::new(
            1,
            self.mux_bit() as usize,
        );
        [
            self.direction
                .map(|v| (0xc8 + group, field.val((v == GpioDirection::Output) as u8))),
            self.inverted.map(|v| (0xb0 + group, field.val(v as u8))),
            self.pull_up
                .map(|v| (0xb8 + group, field.val((v == PullUp::Enabled) as u8))),
            Some((
                0xc0 + group,
                field.val((self.function == GpioFunction::SimpleIo) as u8),
            )),
            Some((
                if self.pin >= 50 { 0x29 } else { 0x25 + group },
                mux.val(self.gpio_selected() as u8),
            )),
            (C::EXTENDED_GPIO1 && self.pin <= 14)
                .then(|| (0x2a, field.val(self.gpio_selected() as u8))),
        ]
        .into_iter()
        .flatten()
    }
}

fn validate_pins<C: IteGpioChip>(pins: &[GpioPinConfig<C>]) {
    assert!(
        !pins
            .iter()
            .enumerate()
            .any(|(i, p)| pins[i + 1..].iter().any(|q| p.pin == q.pin))
    );
    // Shared mux bits must not silently reroute omitted pads.
    for group in COUPLED_GROUPS {
        if let Some(first) = pins.iter().find(|p| group.contains(&p.pin)) {
            assert!(group.iter().all(|pin| {
                pins.iter()
                    .any(|p| p.pin == *pin && p.gpio_selected() == first.gpio_selected())
            }));
        }
    }
}

/// Prepare all members of a coupled group before switching its shared selector.
/// Independent pins retain their existing per-pin write order.
fn settings_for_pins<C: IteGpioChip>(
    pins: &[GpioPinConfig<C>],
) -> impl Iterator<
    Item = (
        u8,
        tock_registers::fields::FieldValue<u8, GPIO_PINS::Register>,
    ),
> {
    pins.iter()
        .flat_map(|pin| {
            pin.settings().filter(move |(index, _)| {
                !(*index == 0x29 && COUPLED_GROUPS.iter().any(|group| group.contains(&pin.pin)))
            })
        })
        .chain(COUPLED_GROUPS.into_iter().filter_map(move |group| {
            pins.iter()
                .find(|pin| group.contains(&pin.pin))
                .and_then(|pin| pin.settings().find(|(index, _)| *index == 0x29))
        }))
}

impl<C: IteGpioChip> SuperIo<C> {
    /// Configure physical pins with field-preserving updates. No GPIO CR30
    /// activation is assumed. Validate the whole request before touching I/O.
    pub fn configure_gpio(&mut self, pins: &[GpioPinConfig<C>], io_base: u16) {
        assert!(io_base < 0x1000);
        validate_pins(pins);
        let uart_gpio = pins.iter().any(|p| p.pin >= 63 && p.gpio_selected());
        let keyboard_gpio = pins
            .iter()
            .any(|p| matches!(p.pin, 56 | 57 | 60..=62) && p.gpio_selected());
        assert!(!uart_gpio || self.config.com2.is_none());
        assert!(!keyboard_gpio || (self.config.keyboard.is_none() && self.config.mouse.is_none()));
        self.enter_config();
        if uart_gpio {
            self.logical_device(2).set_enabled(false);
        }
        if keyboard_gpio {
            self.logical_device(5).set_enabled(false);
            self.logical_device(6).set_enabled(false);
        }
        self.logical_device(7)
            .set_io_base(IoResource::Secondary(io_base));
        for (index, value) in settings_for_pins(pins) {
            self.register::<GPIO_PINS::Register>(index).modify(value);
        }
        self.exit_config();
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    type Old = GpioPinConfig<crate::ite8720f::Ite8720fChip>;
    type New = GpioPinConfig<crate::ite8718f::Ite8718fChip>;

    #[test]
    fn direction_precedes_mux_and_updates_preserve_neighbours() {
        let mut registers = [0xa5; 256];
        let indices = settings_for_pins(&[Old::input(40).pull_up(PullUp::Enabled)])
            .map(|(index, value)| {
                registers[index as usize] = value.modify(registers[index as usize]);
                index
            })
            .collect::<std::vec::Vec<_>>();
        assert_eq!(indices, [0xcb, 0xbb, 0xc3, 0x28]);
        assert_eq!(registers[0xcb], 0xa4);
        assert_eq!(registers[0xbb], 0xa5);
        assert_eq!(registers[0xc3], 0xa5);
        assert_eq!(registers[0x28], 0xa5);
        for (index, value) in Old::peripheral(40).settings() {
            registers[index as usize] = value.modify(registers[index as usize]);
        }
        assert_eq!(registers[0xc3], 0xa4);
        assert_eq!(registers[0x28], 0xa4);
        assert_eq!(registers[0xcb], 0xa4);
        assert_eq!(registers[0xc2], 0xa5);
    }

    #[test]
    fn chip_specific_extended_mux_and_inversion_are_separate_fields() {
        let values = New::input(12)
            .inverted(true)
            .settings()
            .map(|(i, v)| (i, v.value))
            .collect::<std::vec::Vec<_>>();
        assert_eq!(
            values,
            [(0xc8, 0), (0xb0, 4), (0xc0, 4), (0x25, 4), (0x2a, 4)]
        );
        assert_eq!(Old::simple_io(12).settings().count(), 2);
        let values = New::peripheral(12)
            .settings()
            .map(|(i, v)| (i, v.modify(0xff)))
            .collect::<std::vec::Vec<_>>();
        assert_eq!(values, [(0xc0, 0xfb), (0x25, 0xfb), (0x2a, 0xfb)]);
    }

    #[test]
    fn physical_pins_map_to_groups_and_coupled_muxes() {
        for (pin, simple, mux, mask, mux_mask) in [
            (10, 0xc0, 0x25, 1, 1),
            (17, 0xc0, 0x25, 128, 128),
            (20, 0xc1, 0x26, 1, 1),
            (27, 0xc1, 0x26, 128, 128),
            (30, 0xc2, 0x27, 1, 1),
            (37, 0xc2, 0x27, 128, 128),
            (40, 0xc3, 0x28, 1, 1),
            (46, 0xc3, 0x28, 64, 64),
            (50, 0xc4, 0x29, 1, 1),
            (57, 0xc4, 0x29, 128, 64),
            (60, 0xc5, 0x29, 1, 64),
            (67, 0xc5, 0x29, 128, 128),
        ] {
            let mut values = New::simple_io(pin).settings();
            assert_eq!(
                values.next().map(|(i, v)| (i, v.value)),
                Some((simple, mask))
            );
            assert_eq!(
                values.next().map(|(i, v)| (i, v.value)),
                Some((mux, mux_mask))
            );
        }
        validate_pins(&[
            New::input(63),
            New::input(64),
            New::input(65),
            New::input(66),
            New::input(67),
        ]);
    }

    #[test]
    fn coupled_muxes_wait_for_all_requested_controls() {
        let pins = [63, 56, 64, 57, 65, 60, 66, 61, 67, 62].map(New::input);
        validate_pins(&pins);
        let mut registers = [0xff; 256];
        registers[0x29] = 1;
        registers[0xc4] = 0x3f;
        registers[0xc5] = 0;
        let mut switches = 0;
        for (index, value) in settings_for_pins(&pins) {
            if index == 0x29 {
                // Both coupled groups must be prepared, despite interleaved pins.
                assert_eq!(registers[0xcc], 0x3f);
                assert_eq!(registers[0xcd], 0);
                assert_eq!(registers[0xc4], 0xff);
                assert_eq!(registers[0xc5], 0xff);
                switches += 1;
            }
            registers[index as usize] = value.modify(registers[index as usize]);
        }
        assert_eq!(switches, 2);
        assert_eq!(registers[0x29], 0xc1);
    }

    #[test]
    fn ps2_pads_reject_nonexistent_pull_ups() {
        for pin in [56, 57, 60, 61] {
            for state in [PullUp::Enabled, PullUp::Disabled] {
                assert!(std::panic::catch_unwind(|| New::input(pin).pull_up(state)).is_err());
            }
        }
        assert!(New::input(62).pull_up(PullUp::Enabled).pull_up.is_some());
        assert!(Old::input(15).pull_up(PullUp::Enabled).pull_up.is_some());
    }

    #[test]
    #[should_panic]
    fn partial_shared_mux_is_rejected() {
        validate_pins(&[New::simple_io(63)]);
    }
    #[test]
    #[should_panic]
    fn reserved_pin_is_rejected() {
        let _ = New::input(15);
    }
    #[test]
    #[should_panic]
    fn unsupported_pull_up_is_rejected() {
        let _ = New::input(22).pull_up(PullUp::Enabled);
    }
    #[test]
    #[should_panic]
    fn old_chip_retains_its_verified_pin_subset() {
        let _ = Old::input(50);
    }
}
