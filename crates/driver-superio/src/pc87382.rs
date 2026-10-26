//! National Semiconductor PC87382 SuperIO driver descriptor.
//!
//! The ThinkPad X61 uses this laptop-side SuperIO/DLPC block at LPC PnP
//! config port `0x164e`.  The generic [`crate::SuperIo`] driver
//! performs common PnP resource programming. The operations below also support
//! the T60 dock path; board families own pin lists, reset/power timing, LPC
//! isolation and serialization. Complete reference GPIO mode images are not
//! inferred field-preserving policies or authority for other NSC controllers.

pub use crate::nsc::GpioPort0;
use crate::{IoResource, SuperIo, SuperIoChip};
use fstart_core::{pio::PioRegister, services::device::DeviceError};
use tock_registers::interfaces::{Readable, Writeable};

/// Supported complete F1h images from the X6/T60 reference paths, not a
/// decoded or field-preserving general-purpose GPIO policy.
#[derive(Clone, Copy, Debug)]
#[repr(u8)]
pub enum GpioMode {
    Input = 0,
    Output = 3,
    InputPullUp = 4,
}

#[derive(Clone, Copy, Debug)]
pub struct GpioPinConfig {
    /// Physical GPIO number (e.g. 20, not the encoded selector 20h).
    pub pin: u8,
    pub mode: GpioMode,
}

tock_registers::register_bitfields![u8,
    DLPC_CONTROL [
        REQUEST OFFSET(0) NUMBITS(3) [Disconnect = 0, Connect = 7],
        CONNECTED OFFSET(3) NUMBITS(1) []
    ],
    GPIO_MODE_IMAGE [VALUE OFFSET(0) NUMBITS(8) []],
    DLPC_ACTIVATION [ENABLED OFFSET(0) NUMBITS(1) []]
];

fn connect_switch(
    control: &(
         impl Readable<T = u8, R = DLPC_CONTROL::Register>
         + Writeable<T = u8, R = DLPC_CONTROL::Register>
     ),
    timeout_ms: u32,
    mut delay_us: impl FnMut(u32),
) -> Result<(), DeviceError> {
    // Complete reference request image. Status does not establish a working
    // dock peripheral, clock freshness, or reset/power sequencing.
    control.write(DLPC_CONTROL::REQUEST::Connect);
    for _ in 0..timeout_ms {
        if control.is_set(DLPC_CONTROL::CONNECTED) {
            return Ok(());
        }
        delay_us(1000);
    }
    if control.is_set(DLPC_CONTROL::CONNECTED) {
        Ok(())
    } else {
        control.write(DLPC_CONTROL::REQUEST::Disconnect);
        Err(DeviceError::InitFailed)
    }
}

fn connected_if_decoded(
    active: bool,
    mapped_base: u16,
    expected_base: u16,
    control: &impl Readable<T = u8, R = DLPC_CONTROL::Register>,
) -> bool {
    active && mapped_base == expected_base && control.is_set(DLPC_CONTROL::CONNECTED)
}

fn validate_pins(pins: &[GpioPinConfig]) -> Result<(), DeviceError> {
    if pins.iter().enumerate().any(|(i, pin)| {
        !matches!(pin.pin, 0..=4 | 20 | 21 | 23)
            || pins.iter().take(i).any(|other| pin.pin == other.pin)
    }) {
        return Err(DeviceError::ConfigError);
    }
    Ok(())
}

pub use crate::{
    CirConfig, ComPortConfig, EcConfig, GpioConfig, KbcConfig, LpcBaseProvider, MouseConfig,
    ParallelConfig, SuperIoConfig,
};

/// Zero-sized chip descriptor for the NSC PC87382.
pub struct Pc87382Chip;

impl SuperIoChip for Pc87382Chip {
    const ENTER_SEQ: &'static [u8] = &[];
    const EXIT_REG: u8 = 0;
    const EXIT_VAL: u8 = 0;
    const EXIT_NOOP: bool = true;
    // The X61 DLPC path is often accessed through already-decoded extended
    // PnP ports.  Skip the generic ID check; coreboot also treats this as a
    // fixed mainboard resource rather than probing by ID here.
    const CHIP_ID: u16 = 0;
    const COM1_LDN: Option<u8> = None;
    const COM2_LDN: Option<u8> = Some(0x02);
    const KBC_LDN: Option<u8> = None;
    const MOUSE_LDN: Option<u8> = None;
    const EC_LDN: Option<u8> = None;
    const GPIO_LDN: Option<u8> = Some(0x07);
    const CIR_LDN: Option<u8> = Some(0x03);
    const PARALLEL_LDN: Option<u8> = None;
}

/// PC87382 SuperIO driver.
pub type Pc87382 = SuperIo<Pc87382Chip>;

/// Board-facing PC87382 config.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pc87382Config(pub SuperIoConfig);

impl core::ops::Deref for Pc87382Config {
    type Target = SuperIoConfig;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl core::ops::DerefMut for Pc87382Config {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<SuperIoConfig> for Pc87382Config {
    fn from(config: SuperIoConfig) -> Self {
        Self(config)
    }
}

impl SuperIo<Pc87382Chip> {
    fn validate_window(&self, base: u16, size: u16) -> Result<(), DeviceError> {
        let config_end = self
            .idx_port()
            .checked_add(1)
            .ok_or(DeviceError::ConfigError)?;
        if base < 0x100
            || base & (size - 1) != 0
            || (base <= config_end && self.idx_port() <= base + (size - 1))
        {
            return Err(DeviceError::ConfigError);
        }
        Ok(())
    }

    /// Program and activate the two-byte DLPC window, then request disconnect.
    /// Caller owns bus decode/isolation and serialization. No resource allocator
    /// or dock reset/power policy is implied.
    pub fn enable_dlpc(&self, base: u16) -> Result<(), DeviceError> {
        self.validate_window(base, 2)?;
        let mut device = self.logical_device(PC87382_DLPC_LDN);
        device.set_enabled(false);
        device.set_io_base(IoResource::Primary(base));
        device.set_enabled(true);
        self.disconnect_dlpc(base)
    }

    /// Request connection after initialization. On timeout, disconnect and
    /// deactivate the switch. Reset/power timing belongs to the board family.
    pub fn connect_dlpc(
        &self,
        base: u16,
        timeout_ms: u32,
        delay_us: impl FnMut(u32),
    ) -> Result<(), DeviceError> {
        self.validate_window(base, 2)?;
        let result = connect_switch(&PioRegister::new(base), timeout_ms, delay_us);
        if result.is_err() {
            self.logical_device(PC87382_DLPC_LDN).set_enabled(false);
        }
        result
    }

    /// Sample status only if the DLPC LDN is active at the requested address.
    /// A disabled/uninitialized window must not turn an unclaimed FFh read
    /// into a false connection. Caller serializes configuration-port access.
    pub fn dlpc_connected(&self, base: u16) -> Result<bool, DeviceError> {
        self.validate_window(base, 2)?;
        self.logical_device(PC87382_DLPC_LDN);
        let active = self
            .register::<DLPC_ACTIVATION::Register>(0x30)
            .is_set(DLPC_ACTIVATION::ENABLED);
        let mapped_base = u16::from_be_bytes([
            self.register::<()>(0x60).get(),
            self.register::<()>(0x61).get(),
        ]);
        Ok(connected_if_decoded(
            active,
            mapped_base,
            base,
            &PioRegister::new(base),
        ))
    }

    /// Bind an already initialized GPIO data window without toggling activation.
    pub fn gpio_port0(&self, base: u16) -> Result<GpioPort0, DeviceError> {
        self.validate_window(base, 16)?;
        GpioPort0::new(base)
    }

    pub fn disconnect_dlpc(&self, base: u16) -> Result<(), DeviceError> {
        self.validate_window(base, 2)?;
        PioRegister::<u8, DLPC_CONTROL::Register>::new(base)
            .write(DLPC_CONTROL::REQUEST::Disconnect);
        Ok(())
    }

    /// Explicit primary-slot GPIO activation. Do not combine this with the
    /// legacy generic GPIO config (which historically uses the secondary slot).
    pub fn enable_gpio(&self, base: u16) -> Result<GpioPort0, DeviceError> {
        self.validate_window(base, 16)?;
        if self.config.gpio.is_some() {
            return Err(DeviceError::ConfigError);
        }
        let gpio = GpioPort0::new(base)?;
        let mut device = self.logical_device(7);
        device.set_enabled(false);
        device.set_io_base(IoResource::Primary(base));
        device.set_enabled(true);
        Ok(gpio)
    }

    /// Apply complete reference mode images to verified pins only. This does
    /// not preserve undecoded F1h fields; no PC87384 lock semantics are inferred.
    /// A readback failure may leave earlier requested pins changed. GPIO decode
    /// must be initialized first; direction-before-data is not glitch-free.
    pub fn configure_gpio_pins(&self, pins: &[GpioPinConfig]) -> Result<(), DeviceError> {
        validate_pins(pins)?;
        if pins.is_empty() {
            return Ok(());
        }
        self.logical_device(7);
        let selector = self.register::<()>(0xf0);
        let mode = self.register::<GPIO_MODE_IMAGE::Register>(0xf1);
        for pin in pins {
            selector.set((pin.pin / 10) * 16 + pin.pin % 10);
            mode.write(GPIO_MODE_IMAGE::VALUE.val(pin.mode as u8));
            if mode.get() != pin.mode as u8 {
                return Err(DeviceError::ConfigError);
            }
        }
        Ok(())
    }

    /// Request the complete reference 14.318 MHz CLKIN policy and wait for ready.
    /// Caller owns config-port decoding and serialization. Like the original
    /// X6/T60 request, this replaces the clock image, not individual fields.
    pub fn start_14mhz_clock(
        &self,
        timeout_ms: u32,
        delay_us: impl FnMut(u32),
    ) -> Result<(), fstart_core::services::device::DeviceError> {
        crate::nsc::start_external_14mhz_clock(
            &self.register::<crate::nsc::CLOCK_CONTROL::Register>(0x29),
            timeout_ms,
            delay_us,
        )
    }
}

/// PC87382 DLPC logical-device number used by the X61 mainboard DLPC block.
pub const PC87382_DLPC_LDN: u8 = 0x19;

#[cfg(test)]
mod tests {
    use super::*;
    use tock_registers::registers::InMemoryRegister;

    #[test]
    fn disconnected_decode_never_samples_an_unclaimed_runtime_port() {
        struct NoRuntimeRead;
        impl Readable for NoRuntimeRead {
            type T = u8;
            type R = DLPC_CONTROL::Register;
            fn get(&self) -> u8 {
                panic!("unclaimed runtime port was read");
            }
        }
        assert!(!connected_if_decoded(false, 0x164c, 0x164c, &NoRuntimeRead));
        assert!(!connected_if_decoded(true, 0xffff, 0x164c, &NoRuntimeRead));
        assert!(!connected_if_decoded(true, 0x164a, 0x164c, &NoRuntimeRead));
        let status = InMemoryRegister::<u8, DLPC_CONTROL::Register>::new(0x0f);
        assert!(connected_if_decoded(true, 0x164c, 0x164c, &status));
        status.set(0);
        assert!(!connected_if_decoded(true, 0x164c, 0x164c, &status));
    }

    #[test]
    fn switch_wait_samples_deadline_and_disconnects_on_timeout() {
        let control = InMemoryRegister::<u8, DLPC_CONTROL::Register>::new(0);
        let mut waits = 0;
        assert!(
            connect_switch(&control, 2, |us| {
                assert_eq!(us, 1000);
                waits += 1;
                if waits == 2 {
                    control.set(0x0f);
                }
            })
            .is_ok()
        );
        assert_eq!(waits, 2);
        assert!(connect_switch(&control, 0, |_| panic!()).is_err());
        assert_eq!(control.get(), 0);
        let mut waits = 0;
        assert!(connect_switch(&control, 3, |_| waits += 1).is_err());
        assert_eq!(waits, 3);
        assert_eq!(control.get(), 0);
    }

    #[test]
    fn invalid_resources_and_pins_fail_before_io() {
        let controller = Pc87382::new_at_base(SuperIoConfig::default(), 0x164e).unwrap();
        for base in [0, 0xff, 0x164d, 0x164e, 0xffff] {
            assert!(controller.enable_dlpc(base).is_err());
            assert!(controller.connect_dlpc(base, 0, |_| panic!()).is_err());
            assert!(controller.disconnect_dlpc(base).is_err());
            assert!(controller.dlpc_connected(base).is_err());
        }
        for base in [0, 0xf0, 0x1640, 0x1681] {
            assert!(controller.enable_gpio(base).is_err());
        }
        assert!(Pc87382::new_at_base(SuperIoConfig::default(), 0xffff).is_err());
        let pin = GpioPinConfig {
            pin: 20,
            mode: GpioMode::InputPullUp,
        };
        assert!(controller.configure_gpio_pins(&[]).is_ok());
        assert!(controller.configure_gpio_pins(&[pin, pin]).is_err());
        assert!(
            controller
                .configure_gpio_pins(&[GpioPinConfig { pin: 22, ..pin }])
                .is_err()
        );
        assert!(validate_pins(&[pin]).is_ok());
    }
}
