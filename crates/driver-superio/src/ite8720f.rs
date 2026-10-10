//! IT8720F GPIO mux, simple I/O, power-good and flash-bank policy.
//!
//! GPIO register meanings follow ITE IT8720F V0.1 sections 8.3 and 8.11.
//! Flash-bank select bit 0 at LDN7/EF follows flashrom's IT87 DualBIOS support.
//! It is deliberately a read/modify/write: the older datasheet does not define
//! EF's other bits. FC is read-only VID input (except its reload strobe); writing
//! a VID value there, as the GA-D510UD reference does, cannot program a voltage.
use crate::{IoResource, IrqResource, SuperIo, SuperIoChip};
use tock_registers::{
    interfaces::{ReadWriteable, Writeable},
    register_bitfields,
};

register_bitfields![u8,
    GPIO_PINS [ PINS OFFSET(0) NUMBITS(8) [] ],
    EXT_MUX [
        SMBUS_ISOLATION_DISABLE OFFSET(7) NUMBITS(1) [],
        K8_SOFTWARE_POWER_DISABLE OFFSET(6) NUMBITS(1) [],
        VID_TURBO OFFSET(5) NUMBITS(1) [],
        FAN_TAC5 OFFSET(4) NUMBITS(1) [], FAN_TAC4 OFFSET(3) NUMBITS(1) [],
        PCIRSTIN OFFSET(2) NUMBITS(1) [], VCCH5V_DIVIDER OFFSET(1) NUMBITS(1) [],
        ATXPG_DIVIDER OFFSET(0) NUMBITS(1) [],
    ],
    BUS_SELECT [
        RESET OFFSET(6) NUMBITS(2) [Rsmrst = 0, Lreset = 1, PowerOk = 2],
        DISABLE OFFSET(5) NUMBITS(1) [], OUTPUT_ENABLE OFFSET(4) NUMBITS(1) [],
        READ_OUTPUT OFFSET(3) NUMBITS(1) [],
        BSEL0 OFFSET(0) NUMBITS(1) [], BSEL1 OFFSET(1) NUMBITS(1) [],
        BSEL2 OFFSET(2) NUMBITS(1) [],
    ],
    WATCHDOG [
        SECONDS OFFSET(7) NUMBITS(1) [], KEYBOARD_RESET OFFSET(6) NUMBITS(1) [],
        SHORT_PERIOD OFFSET(5) NUMBITS(1) [], POWER_OK_RESET OFFSET(4) NUMBITS(1) [],
        IRQ OFFSET(0) NUMBITS(4) [],
    ],
    FLASH_SELECT [ BANK OFFSET(0) NUMBITS(1) [Main = 0, Backup = 1] ],
];

pub struct Ite8720fChip;
impl SuperIoChip for Ite8720fChip {
    const ENTER_SEQ: &'static [u8] = &[0x87, 0x01, 0x55];
    const EXIT_REG: u8 = 2;
    const EXIT_VAL: u8 = 2;
    const CHIP_ID: u16 = 0x8720;
    const COM1_LDN: Option<u8> = Some(1);
    const COM2_LDN: Option<u8> = Some(2);
    const EC_LDN: Option<u8> = Some(4);
    const KBC_LDN: Option<u8> = Some(5);
    const MOUSE_LDN: Option<u8> = Some(6);
    const GPIO_LDN: Option<u8> = Some(7);
    const CIR_LDN: Option<u8> = Some(10);
    const PARALLEL_LDN: Option<u8> = Some(3);

    fn enter_last_byte(base_port: u16) -> Option<u8> {
        Some(if base_port == 0x4e { 0xaa } else { 0x55 })
    }
}
pub type Ite8720f = SuperIo<Ite8720fChip>;

/// Pin mux selection; `Alternate` selects the GPIO block's special-function
/// routing (watchdog, LED, etc.), not the original peripheral function.
#[derive(Debug, Clone, Copy)]
pub enum GpioFunction {
    Peripheral,
    Alternate,
    SimpleIo,
}

#[derive(Debug, Clone, Copy)]
pub enum GpioDirection {
    Input,
    /// Retain the existing output latch; the caller owns its initial value.
    Output,
}

#[derive(Debug, Clone, Copy)]
pub enum PullUp {
    Disabled,
    Enabled,
}

/// One physical pin, numbered as in the datasheet (e.g. 22 means GP22).
/// Omitted pins, directions and pull-ups retain their existing settings.
#[derive(Debug, Clone, Copy)]
pub struct GpioPinConfig {
    pin: u8,
    function: GpioFunction,
    direction: Option<GpioDirection>,
    pull_up: Option<PullUp>,
}

impl GpioPinConfig {
    #[must_use]
    pub const fn new(pin: u8, function: GpioFunction) -> Self {
        // GP45 is reserved. Group 5's coupled pin mux is not exposed here.
        assert!(pin >= 10 && pin <= 47 && pin % 10 < 8 && pin != 45);
        Self {
            pin,
            function,
            direction: None,
            pull_up: None,
        }
    }

    #[must_use]
    pub const fn peripheral(pin: u8) -> Self {
        Self::new(pin, GpioFunction::Peripheral)
    }

    /// Select simple I/O without changing the pin's direction.
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
        // GP20..27 have no internal pull-ups.
        assert!(self.pin / 10 != 2);
        self.pull_up = Some(pull_up);
        self
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
        [
            // Establish directions before switching mux functions.
            self.direction.map(|direction| {
                (
                    0xc8 + group,
                    field.val(matches!(direction, GpioDirection::Output) as u8),
                )
            }),
            self.pull_up.map(|pull_up| {
                (
                    0xb8 + group,
                    field.val(matches!(pull_up, PullUp::Enabled) as u8),
                )
            }),
            Some((
                0xc0 + group,
                field.val(matches!(self.function, GpioFunction::SimpleIo) as u8),
            )),
            Some((
                0x25 + group,
                field.val(!matches!(self.function, GpioFunction::Peripheral) as u8),
            )),
        ]
        .into_iter()
        .flatten()
    }
}

/// Levels to preload into the BSEL latches, without enabling their outputs.
#[derive(Debug, Clone, Copy)]
pub struct BusSelect {
    pub bsel0_high: bool,
    pub bsel1_high: bool,
    pub bsel2_high: bool,
}

impl BusSelect {
    fn fields(self) -> tock_registers::fields::FieldValue<u8, BUS_SELECT::Register> {
        BUS_SELECT::BSEL0.val(self.bsel0_high as u8)
            + BUS_SELECT::BSEL1.val(self.bsel1_high as u8)
            + BUS_SELECT::BSEL2.val(self.bsel2_high as u8)
    }
}

impl Ite8720f {
    /// Configure physical pins using field-level updates. Unlisted pins and
    /// reserved bits are untouched. Validate the whole request before I/O.
    pub fn configure_gpio(&mut self, pins: &[GpioPinConfig], io_base: u16) {
        assert!(io_base < 0x1000);
        assert!(
            !pins
                .iter()
                .enumerate()
                .any(|(index, pin)| pins[index + 1..].iter().any(|other| pin.pin == other.pin))
        );
        self.enter_config();
        self.logical_device(7)
            .set_io_base(IoResource::Secondary(io_base));
        for config in pins {
            for (index, value) in config.settings() {
                self.register::<GPIO_PINS::Register>(index).modify(value);
            }
        }
        self.exit_config();
    }

    /// Select the internal ATX power-good divider rather than the VIN3 input.
    /// Other extended pin functions remain at their reset configuration.
    pub fn use_internal_power_good(&mut self) {
        self.enter_config();
        self.logical_device(7);
        self.register::<EXT_MUX::Register>(0x2c)
            .write(EXT_MUX::ATXPG_DIVIDER::SET);
        self.exit_config();
    }

    /// Load the three BSEL output latches while retaining transparent pass-through
    /// and RSMRST# reset. The latches are not driven onto the pins in this mode.
    pub fn preset_bus_select(&mut self, levels: BusSelect) {
        self.enter_config();
        self.logical_device(7);
        self.register::<BUS_SELECT::Register>(0xe9).write(
            BUS_SELECT::RESET::Rsmrst
                + BUS_SELECT::DISABLE::CLEAR
                + BUS_SELECT::OUTPUT_ENABLE::CLEAR
                + BUS_SELECT::READ_OUTPUT::CLEAR
                + levels.fields(),
        );
        self.exit_config();
    }

    pub fn disable_watchdog(&mut self) {
        self.enter_config();
        self.logical_device(7);
        self.register::<WATCHDOG::Register>(0x72).write(
            WATCHDOG::SECONDS::CLEAR
                + WATCHDOG::KEYBOARD_RESET::CLEAR
                + WATCHDOG::SHORT_PERIOD::CLEAR
                + WATCHDOG::POWER_OK_RESET::CLEAR
                + WATCHDOG::IRQ.val(0),
        );
        self.write_reg(0x73, 0);
        self.write_reg(0x74, 0);
        self.exit_config();
    }

    /// Select the main DualBIOS flash without changing undocumented pin bits.
    pub fn select_main_flash(&mut self) {
        self.enter_config();
        self.logical_device(7);
        self.register::<FLASH_SELECT::Register>(0xef)
            .modify(FLASH_SELECT::BANK::Main);
        self.exit_config();
    }

    /// Disable the parallel port's secondary (ECP) I/O window.
    pub fn disable_parallel_ecp_decode(&mut self) {
        self.enter_config();
        self.logical_device(3).set_io_base(IoResource::Secondary(0));
        self.exit_config();
    }

    pub fn disconnect_hwmon_irq(&mut self) {
        self.enter_config();
        self.logical_device(4).set_irq(IrqResource::Primary(0));
        self.exit_config();
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    #[test]
    fn physical_pin_fields_preserve_neighbours_and_order_direction_before_mux() {
        let pin = GpioPinConfig::input(40).pull_up(PullUp::Enabled);
        let mut registers = [0xa5; 256];
        let indices = pin
            .settings()
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
        assert_eq!(registers[0xc2], 0xa5);

        // Peripheral selection clears both mux levels, retaining direction.
        for (index, value) in GpioPinConfig::peripheral(40).settings() {
            registers[index as usize] = value.modify(registers[index as usize]);
        }
        assert_eq!(registers[0xc3], 0xa4);
        assert_eq!(registers[0x28], 0xa4);
        assert_eq!(registers[0xcb], 0xa4);
    }

    #[test]
    fn physical_pin_numbers_select_the_right_group_and_bit() {
        for (pin, mux, simple, expected) in [
            (10, 0x25, 0xc0, 0x01),
            (17, 0x25, 0xc0, 0x80),
            (20, 0x26, 0xc1, 0x01),
            (27, 0x26, 0xc1, 0x80),
            (30, 0x27, 0xc2, 0x01),
            (37, 0x27, 0xc2, 0x80),
            (40, 0x28, 0xc3, 0x01),
            (46, 0x28, 0xc3, 0x40),
            (47, 0x28, 0xc3, 0x80),
        ] {
            let settings = GpioPinConfig::simple_io(pin)
                .settings()
                .map(|(index, value)| (index, value.value))
                .collect::<std::vec::Vec<_>>();
            assert_eq!(settings, [(simple, expected), (mux, expected)]);
        }
    }

    #[test]
    fn simple_io_can_retain_direction_and_pull_up_defaults() {
        let mut settings = GpioPinConfig::simple_io(22).settings();
        let (index, value) = settings.next().unwrap();
        assert_eq!(index, 0xc1);
        assert_eq!(value.modify(0), 0x04);
        let (index, value) = settings.next().unwrap();
        assert_eq!(index, 0x26);
        assert_eq!(value.modify(0), 0x04);
        assert!(settings.next().is_none());
    }

    #[test]
    #[should_panic]
    fn reserved_pin_is_rejected() {
        let _ = GpioPinConfig::input(45);
    }

    #[test]
    #[should_panic]
    fn unsupported_pull_up_is_rejected() {
        let _ = GpioPinConfig::input(22).pull_up(PullUp::Enabled);
    }

    #[test]
    fn bsel_lines_encode_independently() {
        for (levels, expected) in [
            (
                BusSelect {
                    bsel0_high: true,
                    bsel1_high: false,
                    bsel2_high: false,
                },
                1,
            ),
            (
                BusSelect {
                    bsel0_high: false,
                    bsel1_high: true,
                    bsel2_high: false,
                },
                2,
            ),
            (
                BusSelect {
                    bsel0_high: false,
                    bsel1_high: false,
                    bsel2_high: true,
                },
                4,
            ),
        ] {
            assert_eq!(levels.fields().value, expected);
        }
    }
}
