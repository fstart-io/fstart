//! PC87384 UART/parallel resources and the GPIO subset used by ThinkPad docks.
//!
//! Configuration registers are always accessible. Like existing NSC profiles,
//! this profile relies on fixed board routing, not generic Winbond-style IDs.
//! Register/pin meanings below follow coreboot's PC87384 header and T60 dock
//! driver. The available 2006 product brief is not a register specification.

pub use crate::nsc::GpioPort0;
use crate::{SuperIo, SuperIoChip};
use fstart_core::services::device::DeviceError;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

pub use crate::{ComPortConfig, GpioConfig, ParallelConfig, SuperIoConfig};

pub struct Pc87384Chip;
impl SuperIoChip for Pc87384Chip {
    const ENTER_SEQ: &'static [u8] = &[];
    const EXIT_REG: u8 = 0;
    const EXIT_VAL: u8 = 0;
    const EXIT_NOOP: bool = true;
    const CHIP_ID: u16 = 0;
    const COM1_LDN: Option<u8> = Some(3);
    const COM2_LDN: Option<u8> = Some(2);
    const PARALLEL_LDN: Option<u8> = Some(1);
    const GPIO_LDN: Option<u8> = Some(7);
    const KBC_LDN: Option<u8> = None;
    const MOUSE_LDN: Option<u8> = None;
    const EC_LDN: Option<u8> = None;
    const CIR_LDN: Option<u8> = None;

    fn gpio_io_resource(base: u16) -> crate::IoResource {
        crate::IoResource::Primary(base)
    }

    fn validate_config(config: &SuperIoConfig) -> Result<(), DeviceError> {
        if config.keyboard.is_some()
            || config.mouse.is_some()
            || config.env_controller.is_some()
            || config.cir.is_some()
            || config
                .gpio
                .is_some_and(|gpio| !valid_gpio_base(gpio.io_base))
        {
            return Err(DeviceError::ConfigError);
        }
        let ports = [
            config.com1.map(|com| (com.io_base, com.irq)),
            config.com2.map(|com| (com.io_base, com.irq)),
            config
                .parallel
                .map(|parallel| (parallel.io_base, parallel.irq)),
        ];
        if ports
            .iter()
            .flatten()
            .any(|&(base, irq)| !(0x100..=0x7f8).contains(&base) || base & 7 != 0 || irq >= 16)
            || ports.iter().enumerate().any(|(i, port)| {
                port.is_some_and(|(base, _)| {
                    ports
                        .iter()
                        .take(i)
                        .flatten()
                        .any(|&(other, _)| base == other)
                })
            })
            || config.gpio.is_some_and(|gpio| {
                ports
                    .iter()
                    .flatten()
                    .any(|&(base, _)| base >= gpio.io_base && base - gpio.io_base < 16)
            })
            || config.com1.iter().chain(config.com2.iter()).any(|com| {
                com.baud_rate == 0
                    || 115200 % com.baud_rate != 0
                    || 115200 / com.baud_rate > u16::MAX as u32
            })
        {
            return Err(DeviceError::ConfigError);
        }
        match config.console_port.as_deref() {
            None => Ok(()),
            Some("com1") if config.com1.is_some() => Ok(()),
            Some("com2") if config.com2.is_some() => Ok(()),
            _ => Err(DeviceError::ConfigError),
        }
    }
}
pub type Pc87384 = SuperIo<Pc87384Chip>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpioDirection {
    Input,
    Output,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpioDrive {
    OpenDrain,
    PushPull,
}

/// Physical GP00–GP07 only. Unlisted pins and event-trigger policy are retained.
#[derive(Debug, Clone, Copy)]
pub struct GpioPinConfig {
    pub pin: u8,
    pub direction: GpioDirection,
    pub drive: GpioDrive,
    pub pull_up: bool,
    pub debounce: bool,
}

tock_registers::register_bitfields![u8,
    PIN_MODE [
        OUTPUT_ENABLE OFFSET(0) NUMBITS(1) [],
        PUSH_PULL OFFSET(1) NUMBITS(1) [],
        PULL_UP OFFSET(2) NUMBITS(1) [],
        LOCK OFFSET(3) NUMBITS(1) [],
        DEBOUNCE OFFSET(6) NUMBITS(1) []
    ],
    PIN_IRQ [ASSERT_IRQ OFFSET(0) NUMBITS(1) []],
    EVENT_IMAGE [BITS OFFSET(0) NUMBITS(8) []]
];

fn pin_fields(pin: &GpioPinConfig) -> tock_registers::fields::FieldValue<u8, PIN_MODE::Register> {
    PIN_MODE::OUTPUT_ENABLE.val(u8::from(pin.direction == GpioDirection::Output))
        + PIN_MODE::PUSH_PULL.val(u8::from(pin.drive == GpioDrive::PushPull))
        + PIN_MODE::PULL_UP.val(u8::from(pin.pull_up))
        + PIN_MODE::DEBOUNCE.val(u8::from(pin.debounce))
}

fn valid_gpio_base(base: u16) -> bool {
    base >= 0x100 && base & 0xf == 0
}

fn validate_pins(pins: &[GpioPinConfig]) -> Result<(), DeviceError> {
    if pins
        .iter()
        .enumerate()
        .any(|(i, pin)| pin.pin > 7 || pins.iter().take(i).any(|p| p.pin == pin.pin))
    {
        Err(DeviceError::ConfigError)
    } else {
        Ok(())
    }
}

impl SuperIo<Pc87384Chip> {
    /// Program validated LDN resources without console logging or 16550 line
    /// initialization. Suitable for SMM reconnect; caller owns clock/pad setup.
    /// Uses the same PnP programmers as generic initialization.
    pub fn configure_resources(&self) -> Result<(), DeviceError> {
        Pc87384Chip::validate_config(&self.config)?;
        if let Some(com) = self.config.com1 {
            self.program_com(3, &com);
        }
        if let Some(com) = self.config.com2 {
            self.program_com(2, &com);
        }
        if let Some(parallel) = self.config.parallel {
            self.program_simple(1, parallel.io_base, parallel.irq);
        }
        if let Some(gpio) = &self.config.gpio {
            self.program_gpio(7, gpio);
        }
        Ok(())
    }

    /// Request the complete reference 14.318 MHz external-clock policy.
    /// This overwrites the clock control image; no unrelated-field preservation
    /// is claimed. Call before enabling UARTs, after decoding the config ports.
    pub fn start_14mhz_clock(
        &self,
        timeout_ms: u32,
        delay_us: impl FnMut(u32),
    ) -> Result<(), DeviceError> {
        crate::nsc::start_external_14mhz_clock(
            &self.register::<crate::nsc::CLOCK_CONTROL::Register>(0x29),
            timeout_ms,
            delay_us,
        )
    }

    /// Quiesce the optional parallel and second-UART LDNs on a console-only
    /// attachment. No unverified floppy/watchdog LDNs are touched.
    pub fn disable_parallel_and_uart2(&self) {
        for ldn in [1, 2] {
            self.logical_device(ldn).set_enabled(false);
        }
    }

    /// Select the complete serial/parallel pad policy used by the T60 dock.
    /// CR22=A9h is reference-backed, not a general or field-preserving mux API.
    /// Caller must establish a PC87384 and the matching external wiring.
    pub fn select_serial_parallel_pads(&self) {
        self.register::<()>(0x22).set(0xa9);
    }

    /// Configure verified bank-0 pins without changing locks/trigger fields.
    /// Disable their IRQ assertion. Validate unsupported/duplicate pins before
    /// I/O and preflight locks before writing any modes. Caller initializes
    /// GPIO resources first; output direction precedes data and is not glitch-free.
    pub fn configure_gpio_pins(&self, pins: &[GpioPinConfig]) -> Result<(), DeviceError> {
        validate_pins(pins)?;
        if pins.is_empty() {
            return Ok(());
        }
        self.gpio_base()?;
        self.logical_device(7);
        let selector = self.register::<()>(0xf0);
        let mode = self.register::<PIN_MODE::Register>(0xf1);
        let irq = self.register::<PIN_IRQ::Register>(0xf2);
        for pin in pins {
            selector.set(pin.pin);
            if mode.is_set(PIN_MODE::LOCK)
                && (pin_fields(pin).modify(mode.get()) != mode.get()
                    || irq.is_set(PIN_IRQ::ASSERT_IRQ))
            {
                return Err(DeviceError::ConfigError);
            }
        }
        for pin in pins {
            selector.set(pin.pin);
            mode.modify(pin_fields(pin));
            irq.modify(PIN_IRQ::ASSERT_IRQ::CLEAR);
        }
        Ok(())
    }

    fn gpio_base(&self) -> Result<u16, DeviceError> {
        let base = self
            .config
            .gpio
            .as_ref()
            .ok_or(DeviceError::ConfigError)?
            .io_base;
        if !valid_gpio_base(base) {
            Err(DeviceError::ConfigError)
        } else {
            Ok(base)
        }
    }

    /// Set a GP00–GP07 output latch, preserving every other latch.
    /// Caller owns direction/pad policy and serializes access to the GPIO block.
    pub fn set_gpio_level(&self, pin: u8, high: bool) -> Result<(), DeviceError> {
        self.gpio_port0()?.set_level(pin, high)
    }

    /// Borrow the configured GPIO bank-0 data window. Caller initializes decode
    /// and direction first and serializes latch updates.
    pub fn gpio_port0(&self) -> Result<GpioPort0, DeviceError> {
        GpioPort0::new(self.gpio_base()?)
    }

    /// Disable/clear the GPIO events using the complete T60 dock reference
    /// images. Affects both event groups, not just configured bank-0 pins.
    /// Undecoded event fields are not claimed preserved; no general GPIO event
    /// routing API or additional physical-pin support is inferred.
    pub fn quiesce_gpio_events(&self) -> Result<(), DeviceError> {
        use fstart_core::pio::PioRegister;
        let base = self.gpio_base()?;
        let port0_enable = PioRegister::<u8, EVENT_IMAGE::Register>::new(base + 2);
        let port0_status_low = PioRegister::<u8, EVENT_IMAGE::Register>::new(base + 3);
        let port0_status_high = PioRegister::<u8, EVENT_IMAGE::Register>::new(base + 4);
        let port1_enable = PioRegister::<u8, EVENT_IMAGE::Register>::new(base + 6);
        let port1_status_low = PioRegister::<u8, EVENT_IMAGE::Register>::new(base + 7);
        let port1_status_high = PioRegister::<u8, EVENT_IMAGE::Register>::new(base + 8);
        port0_enable.write(EVENT_IMAGE::BITS.val(0));
        port0_status_low.write(EVENT_IMAGE::BITS.val(0xff));
        port0_status_high.write(EVENT_IMAGE::BITS.val(0xff));
        port1_enable.write(EVENT_IMAGE::BITS.val(0));
        port1_status_low.write(EVENT_IMAGE::BITS.val(0xff));
        port1_status_high.write(EVENT_IMAGE::BITS.val(0x1f));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_core::services::device::BusDevice;

    fn input(pin: u8) -> GpioPinConfig {
        GpioPinConfig {
            pin,
            direction: GpioDirection::Input,
            drive: GpioDrive::OpenDrain,
            pull_up: true,
            debounce: true,
        }
    }
    #[test]
    fn resource_profile_and_mode_fields_follow_the_reference() {
        assert_eq!(Pc87384Chip::COM1_LDN, Some(3));
        assert_eq!(Pc87384Chip::COM2_LDN, Some(2));
        assert_eq!(Pc87384Chip::PARALLEL_LDN, Some(1));
        assert_eq!(Pc87384Chip::GPIO_LDN, Some(7));
        let registers = core::cell::RefCell::new([0xa5u8; 256]);
        crate::LogicalDevice::select(
            7,
            |reg| registers.borrow()[reg as usize],
            |reg, val| registers.borrow_mut()[reg as usize] = val,
        )
        .set_io_base(Pc87384Chip::gpio_io_resource(0x1620));
        assert_eq!(&registers.borrow()[0x60..0x64], &[0x16, 0x20, 0xa5, 0xa5]);
        assert!(matches!(
            crate::pc87382::Pc87382Chip::gpio_io_resource(0x1680),
            crate::IoResource::Secondary(0x1680)
        ));
        assert_eq!(pin_fields(&input(0)).modify(0xff), 0xfc);
        let output = GpioPinConfig {
            direction: GpioDirection::Output,
            drive: GpioDrive::PushPull,
            pull_up: false,
            debounce: false,
            ..input(1)
        };
        assert_eq!(pin_fields(&output).modify(0xff), 0xbb);
    }
    #[test]
    fn invalid_initialization_fails_before_io() {
        // An empty always-accessible profile must not probe Winbond IDs or
        // issue a fabricated exit command. This host call would fault on I/O.
        Pc87384::new_at_base(SuperIoConfig::default(), 0x2e)
            .unwrap()
            .init()
            .unwrap();
        let com = ComPortConfig {
            io_base: 0x3f8,
            irq: 4,
            baud_rate: 115200,
        };
        let unsupported = SuperIoConfig {
            com1: Some(com),
            keyboard: Some(crate::KbcConfig {
                io_base: 0x60,
                io_ext: 0x64,
                irq: 1,
            }),
            ..SuperIoConfig::default()
        };
        let invalid_gpio = SuperIoConfig {
            gpio: Some(GpioConfig { io_base: 0x1621 }),
            ..SuperIoConfig::default()
        };
        for config in [unsupported, invalid_gpio] {
            assert!(Pc87384::new_at_base(config.clone(), 0x2e).is_err());
            let mut controller = Pc87384::new_at_base(SuperIoConfig::default(), 0x2e).unwrap();
            controller.config = config;
            assert!(controller.init().is_err());
            assert!(controller.configure_resources().is_err());
        }
        assert!(
            Pc87384Chip::validate_config(&SuperIoConfig {
                com1: Some(com),
                com2: Some(com),
                ..SuperIoConfig::default()
            })
            .is_err()
        );
        assert!(
            Pc87384Chip::validate_config(&SuperIoConfig {
                com1: Some(com),
                gpio: Some(GpioConfig { io_base: 0x3f0 }),
                ..SuperIoConfig::default()
            })
            .is_err()
        );
    }

    #[test]
    fn invalid_policy_fails_before_io() {
        let controller = Pc87384::new_at_base(SuperIoConfig::default(), 0x2e).unwrap();
        assert!(controller.configure_gpio_pins(&[input(8)]).is_err());
        assert!(
            controller
                .configure_gpio_pins(&[input(1), input(1)])
                .is_err()
        );
        assert!(controller.set_gpio_level(8, true).is_err());
        assert!(controller.set_gpio_level(1, true).is_err());
        assert!(controller.configure_gpio_pins(&[]).is_ok());
    }
}
