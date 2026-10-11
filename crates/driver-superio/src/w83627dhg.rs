//! Winbond W83627DHG resources and physical GPIO pins.
//!
//! Register definitions follow W83627DHG V1.4 §§20.1 and 20.10. Unlike
//! IT8720F, this chip has no dedicated BSEL controller: boards may wire BSEL
//! signals to ordinary GPIOs. CR2C[4] is a read-only ACPI strap, not policy.

use crate::{DmaResource, IoResource, IrqResource, SuperIo, SuperIoChip};
use tock_registers::{
    fields::{Field, FieldValue},
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

register_bitfields![u8,
    PIN_BITS [ PINS OFFSET(0) NUMBITS(8) [] ],
    MULTIFUNCTION [
        GP34_RESET OFFSET(7) NUMBITS(1) [],
        GP33_RESET OFFSET(6) NUMBITS(1) [],
        GP32_RESET OFFSET(5) NUMBITS(1) [],
        ACPI_STRAP OFFSET(4) NUMBITS(1) [],
        VID_GTL OFFSET(3) NUMBITS(1) [],
        THERMAL_SHUTDOWN OFFSET(2) NUMBITS(1) [],
        UART_B_PINS OFFSET(0) NUMBITS(2) [Gpio = 2, Uart = 3],
    ],
    SPI_CONFIG [ HWM_SMBUS OFFSET(1) NUMBITS(1) [] ],
    ACPI_POWER [ DRAM_STANDBY_GATE OFFSET(4) NUMBITS(1) [] ],
];

pub struct W83627dhgChip;

impl SuperIoChip for W83627dhgChip {
    const ENTER_SEQ: &'static [u8] = &[0x87, 0x87];
    const EXIT_REG: u8 = 0;
    const EXIT_VAL: u8 = 0;
    const EXIT_RAW: Option<u8> = Some(0xaa);
    const CHIP_ID: u16 = 0xa020;
    const CHIP_ID_MASK: u16 = 0xfff0;
    const COM1_LDN: Option<u8> = Some(2);
    const COM2_LDN: Option<u8> = Some(3);
    const KBC_LDN: Option<u8> = Some(5);
    const MOUSE_LDN: Option<u8> = Some(5);
    const MOUSE_IRQ_SECONDARY: bool = true;
    const EC_LDN: Option<u8> = None;
    // GPIOs use virtual activation bits, not generic base/enable semantics.
    const GPIO_LDN: Option<u8> = None;
    const CIR_LDN: Option<u8> = None;
    const PARALLEL_LDN: Option<u8> = Some(1);
}

pub type W83627dhg = SuperIo<W83627dhgChip>;

/// Requested logical value. Inversion, if selected, also inverts the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpioDirection {
    Input,
    Output(bool),
}

/// GPIOs with directly verified mux support: GP32–34, GP40–47, GP50–57.
/// GPIO4 shares a bank-wide mux with UART B; it cannot coexist with COM2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpioPinConfig {
    pin: u8,
    direction: GpioDirection,
    inverted: bool,
}

impl GpioPinConfig {
    #[must_use]
    pub const fn input(pin: u8) -> Self {
        assert!(matches!(pin, 32..=34 | 40..=47 | 50..=57));
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

    fn settings(self) -> impl Iterator<Item = (u8, FieldValue<u8, PIN_BITS::Register>)> {
        let direction = match self.pin / 10 {
            3 => 0xf0,
            4 => 0xf4,
            5 => 0xe0,
            _ => unreachable!(),
        };
        let field = Field::<u8, PIN_BITS::Register>::new(1, (self.pin % 10) as usize);
        [
            // §20.10: data writes to input pins are ignored. Direction first.
            Some((
                direction,
                field.val(matches!(self.direction, GpioDirection::Input) as u8),
            )),
            Some((direction + 2, field.val(self.inverted as u8))),
            match self.direction {
                GpioDirection::Output(high) => Some((direction + 1, field.val(high as u8))),
                GpioDirection::Input => None,
            },
            // GPIO4 watchdog/SUSLED routing is per pin, in addition to CR2C.
            (self.pin / 10 == 4).then_some((0xf7, field.val(0))),
            (self.pin / 10 == 5).then_some((0x2d, field.val(1))),
        ]
        .into_iter()
        .flatten()
    }
}

/// Separate strap-affecting data/mux updates from ordinary GPIO initialization.
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

impl W83627dhg {
    /// Configure simple GPIOs without replacing unlisted pins or strap bits.
    /// Return data/inversion/mux changes separately from direction/activation.
    /// Strap callers can avoid resetting merely to repeat bank initialization.
    ///
    /// GPIO4 selects its entire bank instead of UART B, so COM2 must be absent.
    /// This is initialization, not a glitch-free runtime output transition:
    /// direction must precede data on DHG. Strap users must run on cold boot
    /// and reset after data/routing changes, never reprogram CPU straps on S3 resume.
    pub fn configure_gpio(&mut self, pins: &[GpioPinConfig]) -> GpioUpdate {
        assert!(
            !pins
                .iter()
                .enumerate()
                .any(|(i, pin)| pins[i + 1..].iter().any(|other| pin.pin == other.pin))
        );
        let gpio4 = pins.iter().any(|pin| pin.pin / 10 == 4);
        assert!(!gpio4 || self.config.com2.is_none());
        if pins.is_empty() {
            return GpioUpdate::default();
        }
        self.enter_config();
        if gpio4 {
            self.logical_device(3).set_enabled(false);
        }
        self.logical_device(9);
        let mut changed = GpioUpdate::default();
        for pin in pins {
            for (index, value) in pin.settings() {
                let register = self.register::<PIN_BITS::Register>(index);
                let old = register.get();
                let new = value.modify(old);
                changed.record_setting(index, new != old);
                register.set(new);
            }
        }
        if pins.iter().any(|pin| matches!(pin.pin, 32 | 33)) {
            let register = self.register::<SPI_CONFIG::Register>(0x2a);
            changed.output_or_routing_changed |= register.is_set(SPI_CONFIG::HWM_SMBUS);
            register.modify(SPI_CONFIG::HWM_SMBUS::CLEAR);
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
            let register = self.register::<MULTIFUNCTION::Register>(0x2c);
            let old = register.get();
            changed.output_or_routing_changed |= fields.modify(old) != old;
            register.modify(fields);
        }
        // Activate only the requested GPIO banks, preserving other functions.
        for group in 3..=5 {
            if pins.iter().any(|pin| pin.pin / 10 == group) {
                let field = Field::<u8, PIN_BITS::Register>::new(1, (group - 2) as usize);
                let register = self.register::<PIN_BITS::Register>(0x30);
                changed.direction_or_activation_changed |= register.read(field) == 0;
                self.logical_device(9).set_activation_bit(group - 2, true);
            }
        }
        self.exit_config();
        changed
    }

    /// UART B shares all eight GPIO4 pins. Call only on boards wired for UART.
    pub fn select_uart_b_pins(&mut self) {
        self.enter_config();
        self.register::<MULTIFUNCTION::Register>(0x2c)
            .modify(MULTIFUNCTION::UART_B_PINS::Uart);
        self.exit_config();
    }

    /// Select the GTL/TTL VID input threshold; this does not drive VID outputs.
    pub fn set_vid_input_gtl(&mut self, gtl: bool) {
        self.enter_config();
        self.register::<MULTIFUNCTION::Register>(0x2c)
            .modify(MULTIFUNCTION::VID_GTL.val(gtl as u8));
        self.exit_config();
    }

    /// Enable the standby DRAM power gate without replacing power-loss policy.
    pub fn enable_dram_standby_gate(&mut self) {
        self.enter_config();
        self.logical_device(10).set_irq(IrqResource::Primary(0));
        self.register::<ACPI_POWER::Register>(0xe4)
            .modify(ACPI_POWER::DRAM_STANDBY_GATE::SET);
        self.logical_device(10).set_enabled(true);
        self.exit_config();
    }

    /// Allocate the floppy controller's standard PnP resources.
    pub fn enable_floppy(&mut self, base: u16, irq: u8, dma: u8) {
        self.enter_config();
        let mut device = self.logical_device(0);
        device.set_enabled(false);
        device.set_io_base(IoResource::Primary(base));
        device.set_irq(IrqResource::Primary(irq));
        device.set_dma(DmaResource::Primary(dma));
        device.set_enabled(true);
        self.exit_config();
    }

    /// Set the parallel controller's ECP DMA channel without changing its mode.
    pub fn set_parallel_dma(&mut self, channel: u8) {
        self.enter_config();
        self.logical_device(1)
            .set_dma(DmaResource::Primary(channel));
        self.exit_config();
    }

    /// Disable the unconnected SPI logical device, not the GPIO mux registers.
    pub fn disable_spi(&mut self) {
        self.enter_config();
        self.logical_device(6).set_enabled(false);
        self.exit_config();
    }

    /// GPIO6 has a virtual activation bit; retain its strapped pin directions.
    pub fn enable_gpio6(&mut self) {
        self.enter_config();
        self.logical_device(7).set_activation_bit(3, true);
        self.exit_config();
    }

    /// Disable the watchdog counter, not merely its PnP activation.
    pub fn disable_watchdog(&mut self) {
        self.enter_config();
        self.logical_device(8).write(0xf6, 0);
        self.logical_device(8).set_enabled(false);
        self.exit_config();
    }

    /// Allocate the single-base hardware-monitor window with no IRQ.
    pub fn enable_hwmon(&mut self, base: u16) {
        self.enter_config();
        let mut device = self.logical_device(11);
        device.set_enabled(false);
        device.set_io_base(IoResource::Primary(base));
        device.set_irq(IrqResource::Primary(0));
        device.set_enabled(true);
        self.exit_config();
    }

    pub fn disable_floppy(&mut self) {
        self.enter_config();
        self.logical_device(0).set_enabled(false);
        self.exit_config();
    }
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
    fn dhg_revision_and_shared_keyboard_mouse_resources() {
        for id in 0xa020..=0xa02f {
            assert_eq!(id & W83627dhgChip::CHIP_ID_MASK, W83627dhgChip::CHIP_ID);
        }
        assert_eq!(W83627dhgChip::KBC_LDN, W83627dhgChip::MOUSE_LDN);
        const { assert!(W83627dhgChip::MOUSE_IRQ_SECONDARY) };
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
    fn mux_changes_never_treat_read_only_acpi_strap_as_policy() {
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
    #[should_panic]
    fn reserved_or_unverified_pin_rejected() {
        let _ = GpioPinConfig::input(38);
    }
}
