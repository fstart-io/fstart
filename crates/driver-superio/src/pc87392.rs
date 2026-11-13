//! National Semiconductor PC87392 SuperIO driver descriptor.
//!
//! The ThinkPad X61 dock exposes a PC87392 at LPC PnP config port `0x2e`.
//! This descriptor maps the logical devices used by coreboot's X61 dock
//! configuration, including the dock WDT LDN so board code can explicitly keep
//! it disabled.

use crate::{SuperIo, SuperIoChip};

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

use fstart_core::services::device::DeviceError;
use tock_registers::interfaces::Writeable;

/// Complete GPIO mode images used by the reference LPC dock configuration.
#[derive(Clone, Copy, Debug)]
#[repr(u8)]
pub enum GpioMode {
    InputPullUp = 0x04,
    InputDebouncedPullUp = 0x44,
    OutputPullUp = 0x05,
    OutputPushPull = 0x03,
}

#[derive(Clone, Copy, Debug)]
pub struct GpioPinConfig {
    /// Physical pin number, e.g. 10 rather than the selector 10h.
    pub pin: u8,
    pub mode: GpioMode,
    pub smi: bool,
}

/// Board-selected complete images for the chip's global pin-function registers.
/// These are not inferred from the presence of a dock or GPIO controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PinMux {
    pub register24: u8,
    pub register25: u8,
    pub register26: u8,
    pub register28: u8,
}

tock_registers::register_bitfields![u8,
    /// Global register 24h (8-bit): complete pin-function selection image.
    PIN_FUNCTION24 [IMAGE OFFSET(0) NUMBITS(8) []],
    /// Global register 25h (8-bit): complete pin-function selection image.
    PIN_FUNCTION25 [IMAGE OFFSET(0) NUMBITS(8) []],
    /// Global register 26h (8-bit): complete pin-function selection image.
    PIN_FUNCTION26 [IMAGE OFFSET(0) NUMBITS(8) []],
    /// Global register 28h (8-bit): complete interrupt-routing image.
    INTERRUPT_ROUTING [IMAGE OFFSET(0) NUMBITS(8) []],
    /// Global register 29h (8-bit): dock clock request and ready status.
    CLOCK_CONTROL [READY OFFSET(3) NUMBITS(1) []],
    /// GPIO LDN register F1h (8-bit): complete reference mode image.
    GPIO_MODE [IMAGE OFFSET(0) NUMBITS(8) []],
    /// GPIO LDN register F2h (8-bit): SMI routing image.
    GPIO_INTERRUPT [SMI OFFSET(1) NUMBITS(1) []]
];

fn validate_gpio(base: u16, pins: &[GpioPinConfig]) -> Result<(), DeviceError> {
    if base < 0x100
        || base & 15 != 0
        || pins.iter().enumerate().any(|(i, p)| {
            !matches!(p.pin, 0..=7 | 10..=17 | 20..=27 | 30..=37)
                || pins.iter().take(i).any(|other| other.pin == p.pin)
        })
    {
        return Err(DeviceError::ConfigError);
    }
    Ok(())
}

impl SuperIo<Pc87392Chip> {
    /// Request the reference 14.318 MHz clock and wait for ready.
    pub fn start_14mhz_clock(
        &self,
        timeout_ms: u32,
        delay_us: impl FnMut(u32),
    ) -> Result<(), DeviceError> {
        crate::nsc::start_external_14mhz_clock(
            &self.register::<CLOCK_CONTROL::Register>(0x29),
            0x06,
            CLOCK_CONTROL::READY,
            timeout_ms,
            delay_us,
        )
    }

    /// Apply complete pin-function images supplied by board policy. This
    /// replaces the registers; the driver does not choose wiring or IRQ routes.
    pub fn configure_pin_mux(&self, mux: &PinMux) {
        self.register::<PIN_FUNCTION24::Register>(0x24)
            .write(PIN_FUNCTION24::IMAGE.val(mux.register24));
        self.register::<PIN_FUNCTION25::Register>(0x25)
            .write(PIN_FUNCTION25::IMAGE.val(mux.register25));
        self.register::<PIN_FUNCTION26::Register>(0x26)
            .write(PIN_FUNCTION26::IMAGE.val(mux.register26));
        self.register::<INTERRUPT_ROUTING::Register>(0x28)
            .write(INTERRUPT_ROUTING::IMAGE.val(mux.register28));
    }

    /// Program board-selected pin modes before activating the GPIO window.
    /// The caller owns LPC decode, pin wiring and reset/power sequencing.
    pub fn enable_gpio(&self, base: u16, pins: &[GpioPinConfig]) -> Result<(), DeviceError> {
        validate_gpio(base, pins)?;
        if base <= self.idx_port().saturating_add(1) && self.idx_port() <= base + 15 {
            return Err(DeviceError::ConfigError);
        }
        let mut gpio = self.logical_device(7);
        gpio.set_io_base(crate::IoResource::Primary(base));
        for pin in pins {
            self.register::<()>(0xf0)
                .set((pin.pin / 10) * 16 + pin.pin % 10);
            self.register::<GPIO_MODE::Register>(0xf1)
                .write(GPIO_MODE::IMAGE.val(pin.mode as u8));
            self.register::<GPIO_INTERRUPT::Register>(0xf2)
                .write(GPIO_INTERRUPT::SMI.val(u8::from(pin.smi)));
        }
        gpio.set_enabled(true);
        Ok(())
    }

    pub fn disable_watchdog(&self) {
        self.logical_device(PC87392_WDT_LDN).set_enabled(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_gpio_policies_fail_before_io() {
        let pin = GpioPinConfig {
            pin: 10,
            mode: GpioMode::InputPullUp,
            smi: false,
        };
        assert!(validate_gpio(0x1620, &[pin]).is_ok());
        assert!(validate_gpio(0x1621, &[pin]).is_err());
        assert!(validate_gpio(0x1620, &[pin, pin]).is_err());
        assert!(validate_gpio(0x1620, &[GpioPinConfig { pin: 8, ..pin }]).is_err());
    }
}
