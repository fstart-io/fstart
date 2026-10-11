//! Winbond W83627THF/THG logical devices.
//!
//! ID 0x828x includes the revision nibble. Keyboard and mouse share LDN 5;
//! their IRQs must occupy separate slots. Hardware monitoring uses an indexed
//! register pair at HWMBASE+5/+6, not the two-base ITE EC layout.
//! THF/THG V1.22 §§7.1/7.3 and §§13–14 define this profile. Revised fan
//! output selection is verified only for E-version ID 0x8285.

pub use crate::w83627_gpio::{GpioDirection, GpioPinConfig, GpioUpdate};
use crate::winbond_hwm::{FanOutputMode, FullSpeedCooling};
use crate::{IoResource, IrqResource, SuperIo, SuperIoChip, w83627_gpio};
use fstart_core::services::DeviceError;
use tock_registers::fields::FieldValue;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::register_bitfields;

register_bitfields![u8,
    GLOBAL_OPTIONS [
        CLOCK OFFSET(6) NUMBITS(1) [Mhz24 = 0, Mhz48 = 1],
        MUST_ONE OFFSET(1) NUMBITS(1) [],
    ],
    FAN_OUTPUTS [
        SYSTEM_PWM OFFSET(4) NUMBITS(1) [],
        CPU_PWM OFFSET(5) NUMBITS(1) [],
        AUXILIARY_PWM OFFSET(6) NUMBITS(1) [],
    ],
    LDN7_ENABLE [
        GPIO1_GAME_MIDI OFFSET(0) NUMBITS(1) [],
        GAME OFFSET(1) NUMBITS(1) [], MIDI OFFSET(2) NUMBITS(1) [],
        GPIO5 OFFSET(3) NUMBITS(1) [],
    ],
    LDN9_ENABLE [ GPIO3 OFFSET(0) NUMBITS(1) [], GPIO4 OFFSET(1) NUMBITS(1) [] ],
    KBC_CONFIG [
        CLOCK OFFSET(6) NUMBITS(2) [Mhz6 = 0, Mhz8 = 1, Mhz12 = 2, Mhz16 = 3],
        PORT92 OFFSET(2) NUMBITS(1) [],
        A20_SPEEDUP OFFSET(1) NUMBITS(1) [],
        RESET_SPEEDUP OFFSET(0) NUMBITS(1) [],
    ],
    UART_CLOCK [ CLOCK OFFSET(0) NUMBITS(2) [Standard = 0] ],
    SERIAL_CONFIG [
        IR_LOCATION OFFSET(6) NUMBITS(1) [UartPins = 0, IrPins = 1],
        IR_MODE OFFSET(3) NUMBITS(3) [Disabled = 0],
        HALF_DUPLEX OFFSET(2) NUMBITS(1) [],
        TX_INVERT OFFSET(1) NUMBITS(1) [],
        RX_INVERT OFFSET(0) NUMBITS(1) [],
    ],
];

pub struct W83627thgChip;

impl SuperIoChip for W83627thgChip {
    const ENTER_SEQ: &'static [u8] = &[0x87, 0x87];
    const EXIT_REG: u8 = 0;
    const EXIT_VAL: u8 = 0;
    const EXIT_RAW: Option<u8> = Some(0xaa);
    const CHIP_ID: u16 = 0x8280;
    const CHIP_ID_MASK: u16 = 0xfff0;
    const COM1_LDN: Option<u8> = Some(2);
    const COM2_LDN: Option<u8> = Some(3);
    const KBC_LDN: Option<u8> = Some(5);
    const MOUSE_LDN: Option<u8> = Some(5);
    const MOUSE_IRQ_SECONDARY: bool = true;
    const EC_LDN: Option<u8> = None;
    const GPIO_LDN: Option<u8> = None;
    const CIR_LDN: Option<u8> = None;
    const PARALLEL_LDN: Option<u8> = Some(1);

    fn chip_init(base: u16) {
        use fstart_core::pio::IndexedPioRegister;
        let ldn = IndexedPioRegister::<()>::new(base, base + 1, 7);
        ldn.set(3);
        IndexedPioRegister::<SERIAL_CONFIG::Register>::new(base, base + 1, 0xf1).modify(
            SERIAL_CONFIG::IR_LOCATION::UartPins
                + SERIAL_CONFIG::IR_MODE::Disabled
                + SERIAL_CONFIG::HALF_DUPLEX::CLEAR
                + SERIAL_CONFIG::TX_INVERT::CLEAR
                + SERIAL_CONFIG::RX_INVERT::CLEAR,
        );
        ldn.set(5);
        IndexedPioRegister::<KBC_CONFIG::Register>::new(base, base + 1, 0xf0).modify(
            KBC_CONFIG::CLOCK::Mhz12
                + KBC_CONFIG::PORT92::CLEAR
                + KBC_CONFIG::A20_SPEEDUP::CLEAR
                + KBC_CONFIG::RESET_SPEEDUP::CLEAR,
        );
    }
}

pub type W83627thg = SuperIo<W83627thgChip>;

#[derive(Debug, Clone, Copy)]
pub enum ClockInput {
    Mhz24,
    Mhz48,
}

fn serial_only_ldn7_fields() -> FieldValue<u8, LDN7_ENABLE::Register> {
    LDN7_ENABLE::GPIO1_GAME_MIDI::CLEAR
        + LDN7_ENABLE::GAME::CLEAR
        + LDN7_ENABLE::MIDI::CLEAR
        + LDN7_ENABLE::GPIO5::CLEAR
}
fn serial_only_ldn9_fields() -> FieldValue<u8, LDN9_ENABLE::Register> {
    LDN9_ENABLE::GPIO3::CLEAR + LDN9_ENABLE::GPIO4::CLEAR
}

fn clock_fields(clock: ClockInput) -> FieldValue<u8, GLOBAL_OPTIONS::Register> {
    let clock = match clock {
        ClockInput::Mhz24 => GLOBAL_OPTIONS::CLOCK::Mhz24,
        ClockInput::Mhz48 => GLOBAL_OPTIONS::CLOCK::Mhz48,
    };
    clock + GLOBAL_OPTIONS::MUST_ONE::SET
}

fn fan_revision_supported(id: u16) -> bool {
    id == 0x8285
}

fn fan_mode_fields(
    cooling: &FullSpeedCooling,
) -> Result<Option<FieldValue<u8, FAN_OUTPUTS::Register>>, DeviceError> {
    if cooling.cpu_secondary.is_some() {
        return Err(DeviceError::ConfigError);
    }
    Ok([
        cooling
            .system
            .map(|mode| FAN_OUTPUTS::SYSTEM_PWM.val(matches!(mode, FanOutputMode::Pwm) as u8)),
        cooling
            .cpu_primary
            .map(|mode| FAN_OUTPUTS::CPU_PWM.val(matches!(mode, FanOutputMode::Pwm) as u8)),
        cooling
            .auxiliary
            .map(|mode| FAN_OUTPUTS::AUXILIARY_PWM.val(matches!(mode, FanOutputMode::Pwm) as u8)),
    ]
    .into_iter()
    .flatten()
    .reduce(|fields, field| fields + field))
}

impl W83627thg {
    /// GP32–34 only. GPIO3 activation/muxes differ from EHG/DHG. Call after
    /// `init()` (normal, non-infrared UART B); ordinary COM2 may coexist.
    /// Unrequested controls are preserved. Direction precedes data; this is
    /// not glitch-free runtime switching. Other THG banks are unverified here.
    pub fn configure_gpio(&mut self, pins: &[GpioPinConfig]) -> GpioUpdate {
        w83627_gpio::configure(self, pins, w83627_gpio::Profile::Thg)
    }

    /// Match the physical oscillator, preserving straps and unrelated fields.
    /// CR24 bit 1 is documented must-one, not board policy.
    pub fn set_clock_input(&mut self, clock: ClockInput) {
        self.enter_config();
        self.register::<GLOBAL_OPTIONS::Register>(0x24)
            .modify(clock_fields(clock));
        self.exit_config();
    }

    /// Select the standard 24 MHz / 13 UART clock on both ports. Call before
    /// UART initialization, after matching the physical oscillator with
    /// `set_clock_input`. Preserve UART B's IR delays and unrelated controls.
    pub fn configure_standard_uart_clocks(&mut self) {
        self.enter_config();
        for ldn in [2, 3] {
            self.logical_device(ldn);
            self.register::<UART_CLOCK::Register>(0xf0)
                .modify(UART_CLOCK::CLOCK::Standard);
        }
        self.exit_config();
    }

    /// Select keyboard-controller A20 acceleration without changing the KBC
    /// clock, Port 92 or keyboard-reset acceleration. Call after `init()`,
    /// which establishes the default software-controlled keyboard policy.
    pub fn set_keyboard_a20_acceleration(&mut self, enabled: bool) {
        self.enter_config();
        self.logical_device(5);
        self.register::<KBC_CONFIG::Register>(0xf0)
            .modify(KBC_CONFIG::A20_SPEEDUP.val(enabled as u8));
        self.exit_config();
    }

    /// Select PWM/DC pin output on verified E-version silicon only. This
    /// neither programs duty/voltage nor disables autonomous fan control.
    /// Call before runtime fan programming; mode takeover is not glitch-free.
    /// Unrequested outputs are untouched; a secondary CPU output is invalid.
    pub fn configure_fan_output_modes(
        &mut self,
        cooling: &FullSpeedCooling,
    ) -> Result<(), DeviceError> {
        let Some(fields) = fan_mode_fields(cooling)? else {
            return Ok(());
        };
        self.enter_config();
        let id = u16::from_be_bytes([
            self.register::<()>(0x20).get(),
            self.register::<()>(0x21).get(),
        ]);
        if !fan_revision_supported(id) {
            self.exit_config();
            return Err(DeviceError::InitFailed);
        }
        self.logical_device(11);
        self.register::<FAN_OUTPUTS::Register>(0xf0).modify(fields);
        self.exit_config();
        Ok(())
    }

    /// Stop the watchdog counter without disabling the unrelated GPIO2 bank.
    pub fn disable_watchdog(&mut self) {
        self.enter_config();
        self.logical_device(8).write(0xf6, 0);
        self.exit_config();
    }

    /// Dedicate a secondary controller to UARTs. Stop its watchdog and
    /// deactivate FDC, parallel, KBC, GPIO1–5, game/MIDI, ACPI and HWM.
    /// UART resources/activation and other configuration fields remain intact;
    /// this does not remux pads or guarantee electrical isolation. Call before
    /// using the primary controller's overlapping legacy resources.
    pub fn disable_non_serial_functions(&mut self) {
        self.enter_config();
        self.logical_device(8).write(0xf6, 0);
        for ldn in [0, 1, 5, 8, 10, 11] {
            self.logical_device(ldn).set_enabled(false);
        }
        self.logical_device(7);
        self.register::<LDN7_ENABLE::Register>(0x30)
            .modify(serial_only_ldn7_fields());
        self.logical_device(9);
        self.register::<LDN9_ENABLE::Register>(0x30)
            .modify(serial_only_ldn9_fields());
        self.exit_config();
    }

    /// Allocate the HWM register block and leave its interrupt disconnected.
    pub fn enable_hwmon(&mut self, base: u16) {
        self.enter_config();
        let mut device = self.logical_device(0x0b);
        device.set_enabled(false);
        device.set_io_base(IoResource::Primary(base));
        device.set_irq(IrqResource::Primary(0));
        device.set_enabled(true);
        self.exit_config();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fan_selection_is_e_version_only_and_does_not_claim_full_speed() {
        assert!(fan_revision_supported(0x8285));
        for id in [0x8283, 0x8284, 0x8286, 0x8865] {
            assert!(!fan_revision_supported(id));
        }
        let cooling = FullSpeedCooling {
            system: Some(FanOutputMode::DcVoltage),
            cpu_primary: Some(FanOutputMode::Pwm),
            auxiliary: None,
            cpu_secondary: None,
        };
        let fields = fan_mode_fields(&cooling).unwrap().unwrap();
        assert_eq!(fields.modify(0xff), 0xef);
        assert_eq!(fields.modify(0), 0x20);
        assert!(matches!(
            fan_mode_fields(&FullSpeedCooling {
                cpu_secondary: Some(FanOutputMode::Pwm),
                ..cooling
            }),
            Err(DeviceError::ConfigError)
        ));
        assert!(
            fan_mode_fields(&FullSpeedCooling {
                system: None,
                cpu_primary: None,
                auxiliary: None,
                cpu_secondary: None
            })
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn secondary_virtual_functions_and_clock_preserve_neighbors() {
        assert_eq!(serial_only_ldn7_fields().modify(0xff), 0xf0);
        assert_eq!(serial_only_ldn9_fields().modify(0xff), 0xfc);
        assert_eq!(clock_fields(ClockInput::Mhz48).modify(0x34), 0x76);
        assert_eq!(clock_fields(ClockInput::Mhz24).modify(0xff), 0xbf);
    }

    #[test]
    fn uart_clock_and_keyboard_a20_policy_preserve_other_controls() {
        assert_eq!(UART_CLOCK::CLOCK::Standard.modify(0xff), 0xfc);
        assert_eq!(UART_CLOCK::CLOCK::Standard.modify(0x0c), 0x0c);
        assert_eq!(KBC_CONFIG::A20_SPEEDUP::SET.modify(0x80), 0x82);
        assert_eq!(KBC_CONFIG::A20_SPEEDUP::CLEAR.modify(0xff), 0xfd);
    }

    #[test]
    fn revisions_share_id_and_mouse_does_not_replace_keyboard_irq() {
        for id in 0x8280..=0x828f {
            assert_eq!(id & W83627thgChip::CHIP_ID_MASK, W83627thgChip::CHIP_ID);
        }
        assert_eq!(W83627thgChip::KBC_LDN, W83627thgChip::MOUSE_LDN);
        const { assert!(W83627thgChip::MOUSE_IRQ_SECONDARY) };
    }
}
