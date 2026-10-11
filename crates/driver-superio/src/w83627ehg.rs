//! Winbond W83627EHG resources and physical pin policy.
//!
//! W83627EHF/EF/EHG/EG V1.3 §7 defines the verified EHG profile. Only
//! 886xh IDs are accepted here; this is not automatic EHF/other-chip support.
//! GPIO activation is virtual, and the hardware monitor has one I/O base.
pub use crate::w83627_gpio::{GpioDirection, GpioPinConfig, GpioUpdate};
use crate::{IoResource, IrqResource, SuperIo, SuperIoChip, w83627_gpio};
use tock_registers::{interfaces::ReadWriteable, register_bitfields};

register_bitfields![u8,
    GLOBAL_OPTIONS [
        CLOCK OFFSET(6) NUMBITS(1) [Mhz24 = 0, Mhz48 = 1],
        SYSTEM_FAN_PUSH_PULL OFFSET(4) NUMBITS(1) [],
        CPU_FAN_PUSH_PULL OFFSET(3) NUMBITS(1) [],
    ],
    PIN_SELECTION [
        UART_A_GPIO6 OFFSET(3) NUMBITS(1) [Uart = 0],
        SECONDARY_CPU_FAN OFFSET(1) NUMBITS(2) [Native = 0],
    ],
    MULTIFUNCTION [
        VID_VRM10 OFFSET(3) NUMBITS(1) [],
        UART_B_PINS OFFSET(0) NUMBITS(2) [Uart = 3],
    ],
];

pub struct W83627ehgChip;
impl SuperIoChip for W83627ehgChip {
    const ENTER_SEQ: &'static [u8] = &[0x87, 0x87];
    const EXIT_REG: u8 = 0;
    const EXIT_VAL: u8 = 0;
    const EXIT_RAW: Option<u8> = Some(0xaa);
    const CHIP_ID: u16 = 0x8860;
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
}
pub type W83627ehg = SuperIo<W83627ehgChip>;

#[derive(Debug, Clone, Copy)]
pub enum ClockInput {
    Mhz24,
    Mhz48,
}
#[derive(Debug, Clone, Copy)]
pub enum FanOutput {
    System,
    CpuPrimary,
}
#[derive(Debug, Clone, Copy)]
pub enum OutputDriver {
    OpenDrain,
    PushPull,
}

fn clock_fields(
    clock: ClockInput,
) -> tock_registers::fields::FieldValue<u8, GLOBAL_OPTIONS::Register> {
    match clock {
        ClockInput::Mhz24 => GLOBAL_OPTIONS::CLOCK::Mhz24,
        ClockInput::Mhz48 => GLOBAL_OPTIONS::CLOCK::Mhz48,
    }
}
fn drive_fields(
    output: FanOutput,
    driver: OutputDriver,
) -> tock_registers::fields::FieldValue<u8, GLOBAL_OPTIONS::Register> {
    let high = matches!(driver, OutputDriver::PushPull) as u8;
    match output {
        FanOutput::System => GLOBAL_OPTIONS::SYSTEM_FAN_PUSH_PULL.val(high),
        FanOutput::CpuPrimary => GLOBAL_OPTIONS::CPU_FAN_PUSH_PULL.val(high),
    }
}

impl W83627ehg {
    /// GP10–17, GP32–34, GP40–47 and GP50–57 only. GPIO1 replaces the
    /// game-port bank, so all eight GPIO1 pads must be explicitly requested.
    /// GPIO4 replaces UART B and requires COM2 absent from the configuration.
    /// Controls for unrequested pads are preserved, not all bank-wide routes.
    /// Direction precedes data; this is not glitch-free runtime switching.
    pub fn configure_gpio(&mut self, pins: &[GpioPinConfig]) -> GpioUpdate {
        w83627_gpio::configure(self, pins, w83627_gpio::Profile::Ehg)
    }
    /// Match the board's actual oscillator without rewriting straps/reserved bits.
    pub fn set_clock_input(&mut self, clock: ClockInput) {
        self.enter_config();
        self.register::<GLOBAL_OPTIONS::Register>(0x24)
            .modify(clock_fields(clock));
        self.exit_config();
    }
    /// Electrical output-buffer selection only, not PWM duty or fan control.
    pub fn set_fan_output_driver(&mut self, output: FanOutput, driver: OutputDriver) {
        self.enter_config();
        self.register::<GLOBAL_OPTIONS::Register>(0x24)
            .modify(drive_fields(output, driver));
        self.exit_config();
    }
    /// Route pins 119/120 to CPUFANIN1/CPUFANOUT1 instead of GPIO2 or MIDI.
    /// Deactivate MIDI without disturbing the other virtual LDN7 functions.
    /// Counter/PWM enable and control remain hardware-monitor policy.
    pub fn select_secondary_cpu_fan_pins(&mut self) {
        self.enter_config();
        self.logical_device(7).set_activation_bit(2, false);
        self.register::<PIN_SELECTION::Register>(0x29)
            .modify(PIN_SELECTION::SECONDARY_CPU_FAN::Native);
        self.exit_config();
    }
    /// Restore UART A instead of the GPIO6 bank. GPIO6 controls are preserved.
    pub fn select_uart_a_pins(&mut self) {
        self.enter_config();
        self.register::<PIN_SELECTION::Register>(0x29)
            .modify(PIN_SELECTION::UART_A_GPIO6::Uart);
        self.exit_config();
    }
    /// Select UART B instead of the entire GPIO4 bank, including its IR pads.
    pub fn select_uart_b_pins(&mut self) {
        self.enter_config();
        self.register::<MULTIFUNCTION::Register>(0x2c)
            .modify(MULTIFUNCTION::UART_B_PINS::Uart);
        self.exit_config();
    }
    /// VID input threshold only: true selects VRM10, false selects TTL.
    pub fn set_vid_input_vrm10(&mut self, enabled: bool) {
        self.enter_config();
        self.register::<MULTIFUNCTION::Register>(0x2c)
            .modify(MULTIFUNCTION::VID_VRM10.val(enabled as u8));
        self.exit_config();
    }
    /// Serial flash uses activation bit 1, unlike the ordinary PnP enable bit.
    pub fn disable_serial_flash(&mut self) {
        self.enter_config();
        self.logical_device(6).set_activation_bit(1, false);
        self.exit_config();
    }
    pub fn disable_floppy(&mut self) {
        self.enter_config();
        self.logical_device(0).set_enabled(false);
        self.exit_config();
    }
    pub fn disable_parallel(&mut self) {
        self.enter_config();
        self.logical_device(1).set_enabled(false);
        self.exit_config();
    }
    /// Stop the actual watchdog counter as well as disabling its output function.
    pub fn disable_watchdog(&mut self) {
        self.enter_config();
        self.logical_device(8).write(0xf6, 0);
        self.logical_device(8).set_enabled(false);
        self.exit_config();
    }
    /// Allocate the single-base hardware-monitor window with no IRQ.
    /// This does not start monitoring or establish a safe fan policy.
    pub fn enable_hwmon(&mut self, base: u16) {
        self.enter_config();
        let mut device = self.logical_device(11);
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
    fn ehg_ids_and_secondary_mouse_irq_are_chip_specific() {
        for id in 0x8860..=0x886f {
            assert_eq!(id & W83627ehgChip::CHIP_ID_MASK, W83627ehgChip::CHIP_ID);
        }
        for id in [0x8850, 0xa020, 0x8718] {
            assert_ne!(id & W83627ehgChip::CHIP_ID_MASK, W83627ehgChip::CHIP_ID);
        }
        assert_eq!(W83627ehgChip::KBC_LDN, W83627ehgChip::MOUSE_LDN);
        const { assert!(W83627ehgChip::MOUSE_IRQ_SECONDARY) };
    }
    #[test]
    fn electrical_options_preserve_keyboard_strap_and_reserved_bits() {
        assert_eq!(clock_fields(ClockInput::Mhz24).modify(0xff), 0xbf);
        assert_eq!(clock_fields(ClockInput::Mhz48).modify(0x84), 0xc4);
        assert_eq!(
            drive_fields(FanOutput::System, OutputDriver::OpenDrain).modify(0xff),
            0xef
        );
        assert_eq!(
            drive_fields(FanOutput::CpuPrimary, OutputDriver::PushPull).modify(0xc4),
            0xcc
        );
        assert_eq!(PIN_SELECTION::SECONDARY_CPU_FAN::Native.modify(0xff), 0xf9);
        assert_eq!(PIN_SELECTION::UART_A_GPIO6::Uart.modify(0xff), 0xf7);
        assert_eq!(MULTIFUNCTION::VID_VRM10::CLEAR.modify(0xff), 0xf7);
    }
}
