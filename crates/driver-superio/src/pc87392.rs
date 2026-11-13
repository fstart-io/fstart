//! National Semiconductor PC87392 SuperIO.
//!
//! The ThinkPad X6 UltraBase carries one at LPC PnP config port `0x2e`. The
//! logical devices, the GPIO pin configuration bits and the 14.318 MHz clock
//! request follow coreboot's `superio/nsc/pc87392` and its X60/X61 dock
//! code. The GPIO block's runtime registers follow the offsets that dock code
//! uses. No datasheet decodes the SuperIO configuration registers 24h-28h;
//! they are written as complete images ([`SioConfig`]).

pub use crate::nsc::{GpioDirection, GpioDrive};
use crate::{IoResource, SuperIo, SuperIoChip};
use fstart_core::{pio::PioRegister, services::device::DeviceError};
use tock_registers::RegisterLongName;
use tock_registers::interfaces::{Readable, Writeable};

pub use crate::{
    CirConfig, ComPortConfig, EcConfig, GpioConfig, KbcConfig, LpcBaseProvider, MouseConfig,
    ParallelConfig, SuperIoConfig,
};

/// Zero-sized chip descriptor for the NSC PC87392.
pub struct Pc87392Chip;

impl SuperIoChip for Pc87392Chip {
    const ENTER_SEQ: &'static [u8] = &[];
    const EXIT_REG: u8 = 0;
    const EXIT_VAL: u8 = 0;
    const EXIT_NOOP: bool = true;
    // The dock-side chip is behind a board-controlled LPC switch and coreboot
    // configures it as a fixed PnP resource.  Skip the generic ID check.
    const CHIP_ID: u16 = 0;
    const COM1_LDN: Option<u8> = Some(0x03);
    const COM2_LDN: Option<u8> = Some(0x02);
    const KBC_LDN: Option<u8> = None;
    const MOUSE_LDN: Option<u8> = None;
    const EC_LDN: Option<u8> = None;
    const GPIO_LDN: Option<u8> = Some(0x07);
    const CIR_LDN: Option<u8> = None;
    const PARALLEL_LDN: Option<u8> = Some(0x01);
}

/// Dock-side PC87392 SuperIO driver.
pub type Pc87392 = SuperIo<Pc87392Chip>;

/// Board-facing PC87392 config.
#[derive(Debug, Clone, Default)]
pub struct Pc87392Config(pub SuperIoConfig);

impl core::ops::Deref for Pc87392Config {
    type Target = SuperIoConfig;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl core::ops::DerefMut for Pc87392Config {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<SuperIoConfig> for Pc87392Config {
    fn from(config: SuperIoConfig) -> Self {
        Self(config)
    }
}

/// PC87392 floppy-controller logical-device number from coreboot's X61 dock config.
pub const PC87392_FDC_LDN: u8 = 0x00;

/// PC87392 watchdog logical-device number from coreboot's `pc87392.h`.
pub const PC87392_WDT_LDN: u8 = 0x0a;

const PARALLEL_LDN: u8 = 0x01;
const COM1_LDN: u8 = 0x03;
const GPIO_LDN: u8 = 0x07;

tock_registers::register_bitfields![u8,
    /// SuperIO configuration 29h: the 14.318 MHz clock request and its
    /// ready flag.
    CLOCK_CONTROL [READY OFFSET(3) NUMBITS(1) []],
    /// GPIO pin configuration (F1h) of the pin F0h selects.
    PIN_CONFIG [
        OUTPUT_ENABLE OFFSET(0) NUMBITS(1) [],
        PUSH_PULL OFFSET(1) NUMBITS(1) [],
        PULL_UP OFFSET(2) NUMBITS(1) [],
        LOCK OFFSET(3) NUMBITS(1) [],
        TRIGGER_LEVEL OFFSET(4) NUMBITS(1) [],
        TRIGGER_LOW OFFSET(5) NUMBITS(1) [],
        DEBOUNCE OFFSET(6) NUMBITS(1) []
    ],
    /// GPIO pin event routing (F2h) of the pin F0h selects.
    PIN_EVENT [
        IRQ OFFSET(0) NUMBITS(1) [],
        SMI OFFSET(1) NUMBITS(1) []
    ]
];

/// The 14.318 MHz clock request image (29h).
const CLOCK_REQUEST: u8 = 0x06;

/// The SuperIO configuration registers 24h, 25h, 26h and 28h, written as
/// complete images in that order.
#[derive(Debug, Clone, Copy)]
pub struct SioConfig {
    /// 24h: the multiplexed pin functions.
    pub pin_functions: u8,
    /// 25h: the parallel port's PNF polarity.
    pub parallel: u8,
    /// 26h: the floppy controller.
    pub fdc: u8,
    /// 28h: where GPIO events are routed.
    pub gpio_routing: u8,
}

/// A GPIO pin's configuration (F1h) and event routing (F2h), written as
/// complete images. Unset fields (lock, event trigger) are written clear.
#[derive(Debug, Clone, Copy)]
pub struct PinMode {
    pub direction: GpioDirection,
    pub drive: GpioDrive,
    pub pull_up: bool,
    pub debounce: bool,
    /// The pin's event asserts SMI.
    pub smi: bool,
}

impl PinMode {
    /// The pin configuration image (F1h).
    #[must_use]
    pub fn config_image(&self) -> u8 {
        (PIN_CONFIG::OUTPUT_ENABLE.val(u8::from(self.direction == GpioDirection::Output))
            + PIN_CONFIG::PUSH_PULL.val(u8::from(self.drive == GpioDrive::PushPull))
            + PIN_CONFIG::PULL_UP.val(u8::from(self.pull_up))
            + PIN_CONFIG::DEBOUNCE.val(u8::from(self.debounce)))
        .value
    }

    /// The pin event routing image (F2h).
    #[must_use]
    pub fn event_image(&self) -> u8 {
        PIN_EVENT::SMI.val(u8::from(self.smi)).value
    }
}

/// The F0h selector of the physical pin GPpb, given as the decimal `pb`
/// (e.g. 16 for GP16): port p 0..=3, bit b 0..=7.
#[must_use]
pub const fn pin_selector(pin: u8) -> Option<u8> {
    let (port, bit) = (pin / 10, pin % 10);
    if port > 3 || bit > 7 {
        None
    } else {
        Some((port << 4) | bit)
    }
}

impl SuperIo<Pc87392Chip> {
    /// Request the 14.318 MHz clock and wait up to `timeout_ms` for it to
    /// settle. The request replaces the whole clock image.
    pub fn start_14mhz_clock(
        &self,
        timeout_ms: u32,
        mut delay_us: impl FnMut(u32),
    ) -> Result<(), DeviceError> {
        let control = self.register::<CLOCK_CONTROL::Register>(0x29);
        control.set(CLOCK_REQUEST);
        for _ in 0..timeout_ms {
            if control.is_set(CLOCK_CONTROL::READY) {
                return Ok(());
            }
            delay_us(1000);
        }
        if control.is_set(CLOCK_CONTROL::READY) {
            Ok(())
        } else {
            Err(DeviceError::InitFailed)
        }
    }

    /// Write the SuperIO configuration images 24h-28h.
    pub fn configure_sio(&self, config: SioConfig) {
        self.write_reg(0x24, config.pin_functions);
        self.write_reg(0x25, config.parallel);
        self.write_reg(0x26, config.fdc);
        self.write_reg(0x28, config.gpio_routing);
    }

    /// Place the GPIO block at its configured base, still inactive, for its
    /// pins to be configured ([`GpioPins`]).
    pub fn place_gpio(&self) -> Result<GpioPins<'_>, DeviceError> {
        let gpio = self.gpio()?;
        self.logical_device(GPIO_LDN)
            .set_io_base(IoResource::Primary(gpio.base));
        Ok(GpioPins { sio: self, gpio })
    }

    /// The configured GPIO block's runtime registers, without I/O.
    pub fn gpio(&self) -> Result<Gpio, DeviceError> {
        let base = self.config.gpio.ok_or(DeviceError::ConfigError)?.io_base;
        if base < 0x100 || base & 0xf != 0 {
            return Err(DeviceError::ConfigError);
        }
        Ok(Gpio { base })
    }

    /// Place and activate the configured parallel port.
    pub fn enable_parallel(&self) -> Result<(), DeviceError> {
        let parallel = self.config.parallel.ok_or(DeviceError::ConfigError)?;
        self.program_simple(PARALLEL_LDN, parallel.io_base, parallel.irq);
        Ok(())
    }

    /// Place and activate the configured COM1.
    pub fn enable_com1(&self) -> Result<(), DeviceError> {
        let com = self.config.com1.ok_or(DeviceError::ConfigError)?;
        self.program_simple(COM1_LDN, com.io_base, com.irq);
        Ok(())
    }

    /// Keep the watchdog off (coreboot's `device pnp 2e.a off`).
    pub fn disable_watchdog(&self) {
        self.logical_device(PC87392_WDT_LDN).set_enabled(false);
    }
}

/// The placed GPIO block, its logical device selected, while its pins are
/// configured. Select no other logical device until [`Self::enable`].
pub struct GpioPins<'a> {
    sio: &'a Pc87392,
    gpio: Gpio,
}

impl GpioPins<'_> {
    /// Configure the physical pin GPpb, given as the decimal `pb`.
    pub fn configure(&self, pin: u8, mode: PinMode) -> Result<(), DeviceError> {
        let selector = pin_selector(pin).ok_or(DeviceError::ConfigError)?;
        self.sio.write_reg(0xf0, selector);
        self.sio.write_reg(0xf1, mode.config_image());
        self.sio.write_reg(0xf2, mode.event_image());
        Ok(())
    }

    /// Activate the GPIO block.
    pub fn enable(self) -> Gpio {
        let activation = self.sio.read_reg(0x30);
        self.sio.write_reg(0x30, activation | 1);
        self.gpio
    }
}

/// The GPIO block's runtime registers, typed with the board's bitfields for
/// its wiring. Ports 0 and 1 have data out, data in, event enable and event
/// status at +0..+3 and +4..+7; ports 2 and 3 have data out and data in at
/// +8/+9 and +Ah/+Bh.
#[derive(Debug, Clone, Copy)]
pub struct Gpio {
    base: u16,
}

impl Gpio {
    // Arithmetic, not a table: SMM handlers reach this and may not address
    // read-only data.
    const fn port(&self, port: u8) -> Result<u16, DeviceError> {
        if port < 2 {
            Ok(self.base + port as u16 * 4)
        } else if port < 4 {
            Ok(self.base + 8 + (port as u16 - 2) * 2)
        } else {
            Err(DeviceError::ConfigError)
        }
    }

    const fn event_port(&self, port: u8) -> Result<u16, DeviceError> {
        if port < 2 {
            self.port(port)
        } else {
            Err(DeviceError::ConfigError)
        }
    }

    /// Port `port`'s data out.
    pub const fn data_out<R: RegisterLongName>(
        &self,
        port: u8,
    ) -> Result<PioRegister<u8, R>, DeviceError> {
        match self.port(port) {
            Ok(base) => Ok(PioRegister::new(base)),
            Err(error) => Err(error),
        }
    }

    /// Port `port`'s data in.
    pub const fn data_in<R: RegisterLongName>(
        &self,
        port: u8,
    ) -> Result<PioRegister<u8, R>, DeviceError> {
        match self.port(port) {
            Ok(base) => Ok(PioRegister::new(base + 1)),
            Err(error) => Err(error),
        }
    }

    /// Port 0's or 1's event enable.
    pub const fn event_enable<R: RegisterLongName>(
        &self,
        port: u8,
    ) -> Result<PioRegister<u8, R>, DeviceError> {
        match self.event_port(port) {
            Ok(base) => Ok(PioRegister::new(base + 2)),
            Err(error) => Err(error),
        }
    }

    /// Port 0's or 1's event status.
    pub const fn event_status<R: RegisterLongName>(
        &self,
        port: u8,
    ) -> Result<PioRegister<u8, R>, DeviceError> {
        match self.event_port(port) {
            Ok(base) => Ok(PioRegister::new(base + 3)),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INPUT: PinMode = PinMode {
        direction: GpioDirection::Input,
        drive: GpioDrive::OpenDrain,
        pull_up: false,
        debounce: false,
        smi: false,
    };

    #[test]
    fn pin_images_follow_coreboot_bits() {
        assert_eq!(pin_selector(37), Some(0x37));
        assert_eq!(pin_selector(0), Some(0));
        assert_eq!(pin_selector(38), None);
        assert_eq!(pin_selector(40), None);
        let debounced = PinMode {
            pull_up: true,
            debounce: true,
            smi: true,
            ..INPUT
        };
        assert_eq!(
            (debounced.config_image(), debounced.event_image()),
            (0x44, 0x02)
        );
        let push_pull = PinMode {
            direction: GpioDirection::Output,
            drive: GpioDrive::PushPull,
            ..INPUT
        };
        assert_eq!(
            (push_pull.config_image(), push_pull.event_image()),
            (0x03, 0)
        );
    }

    #[test]
    fn runtime_registers_follow_the_port_layout() {
        let gpio = Gpio { base: 0x1620 };
        let address = |register: Result<PioRegister<u8>, DeviceError>| register.map(|r| r.port());
        assert_eq!(address(gpio.data_out(0)), Ok(0x1620));
        assert_eq!(address(gpio.data_in(0)), Ok(0x1621));
        assert_eq!(address(gpio.event_enable(0)), Ok(0x1622));
        assert_eq!(address(gpio.event_status(0)), Ok(0x1623));
        assert_eq!(address(gpio.data_out(1)), Ok(0x1624));
        assert_eq!(address(gpio.event_enable(1)), Ok(0x1626));
        assert_eq!(address(gpio.data_out(2)), Ok(0x1628));
        assert_eq!(address(gpio.data_in(2)), Ok(0x1629));
        assert_eq!(address(gpio.data_out(3)), Ok(0x162a));
        assert!(gpio.data_out::<()>(4).is_err());
        assert!(gpio.event_enable::<()>(2).is_err());
        assert!(gpio.event_status::<()>(3).is_err());
    }

    #[test]
    fn invalid_policy_fails_before_io() {
        let without_gpio = Pc87392::new_at_base(SuperIoConfig::default(), 0x2e).unwrap();
        assert!(without_gpio.gpio().is_err());
        assert!(without_gpio.place_gpio().is_err());
        assert!(without_gpio.enable_parallel().is_err());
        assert!(without_gpio.enable_com1().is_err());
    }
}
