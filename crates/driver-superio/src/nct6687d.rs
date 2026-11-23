// SPDX-License-Identifier: MIT OR Apache-2.0
//! Nuvoton NCT6687D resources and paged EC register transport.
//!
//! Resource assignments follow coreboot superio/nuvoton/nct6687d. Linux nct6683 and coreboot's
//! superiotool identify the NCT6687D family with the low ID nibble masked.
//! The EC is NOT the generic two-I/O-base environment controller. Use the
//! separate single-base EC transport and explicit monitor/fan initialization.

use crate::{IoResource, SuperIo, SuperIoChip, SuperIoConfig};
use fstart_core::pio::PioRegister;
use fstart_core::services::device::DeviceError;
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

#[path = "nct6687d/ec.rs"]
pub mod ec;
pub mod hwm;

pub const ACPI_LDN: u8 = 0x0a;
pub const EC_LDN: u8 = 0x0b;
pub const SLEEP_POWER_LDN: u8 = 0x0d;
pub const TACH_PWM_LDN: u8 = 0x0e;

register_bitfields![u8,
    pub AcpiPowerLoss [ POLICY OFFSET(5) NUMBITS(2) [
        AlwaysOff = 0,
        AlwaysOn = 1,
        PreviousState = 2,
        UserDefined = 3
    ] ]
];

pub struct Nct6687dChip;
impl SuperIoChip for Nct6687dChip {
    const ENTER_SEQ: &'static [u8] = &[0x87, 0x87];
    const EXIT_REG: u8 = 0;
    const EXIT_VAL: u8 = 0;
    const EXIT_RAW: Option<u8> = Some(0xaa);
    const CHIP_ID: u16 = 0xd590;
    const CHIP_ID_MASK: u16 = 0xfff0;
    const COM1_LDN: Option<u8> = Some(2);
    const COM2_LDN: Option<u8> = Some(3);
    const KBC_LDN: Option<u8> = Some(5);
    const MOUSE_LDN: Option<u8> = Some(5);
    const MOUSE_IRQ_SECONDARY: bool = true;
    const EC_LDN: Option<u8> = None;
    const GPIO_LDN: Option<u8> = None;
    const CIR_LDN: Option<u8> = Some(6);
    const PARALLEL_LDN: Option<u8> = Some(1);

    fn matches_chip_id(id: u16) -> bool {
        // The M900 reports c73a, which superiotool lists as NCT6685D/NCT6686D
        // with the same relevant global registers and LDN layout. The M900
        // coreboot port uses this driver layout for both parts.
        id & Self::CHIP_ID_MASK == Self::CHIP_ID || id == 0xc73a
    }

    fn validate_config(config: &SuperIoConfig) -> Result<(), DeviceError> {
        if config.env_controller.is_some() || config.gpio.is_some() {
            return Err(DeviceError::ConfigError);
        }
        for base in [
            config.com1.map(|c| c.io_base),
            config.com2.map(|c| c.io_base),
            config.parallel.map(|c| c.io_base),
            config.cir.map(|c| c.io_base),
        ]
        .into_iter()
        .flatten()
        {
            // Source UART/parallel/CIR PNP_IO0 mask 0x0ff8.
            if base == 0 || base & !0x0ff8 != 0 {
                return Err(DeviceError::ConfigError);
            }
        }
        if let Some(kbc) = config.keyboard {
            // Both keyboard resources have the source 12-bit PNP mask.
            for base in [kbc.io_base, kbc.io_ext] {
                if base == 0 || base & !0x0fff != 0 {
                    return Err(DeviceError::ConfigError);
                }
            }
        }
        Ok(())
    }
}
pub type Nct6687d = SuperIo<Nct6687dChip>;
pub type FirmwareEc = ec::Ec<PioRegister<u8>, PioRegister<u8>, PioRegister<u8>>;

/// Board-provided bytes for the chip's global multifunction registers. These
/// are not generic silicon defaults, and the individual pin meanings are not
/// guessed from a different Nuvoton device.
#[derive(Debug, Clone, Copy)]
pub struct PinConfig {
    pub cr13: u8,
    pub cr1d: u8,
    pub cr1f: u8,
    pub cr22: u8,
    pub cr23: u8,
    pub cr24: u8,
    pub cr25: u8,
    pub cr29: u8,
    pub cr2a: u8,
    pub sleep_power_f3: u8,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AfterPowerFailure {
    Off = 0,
    On = 1,
    PreviousState = 2,
}

impl Nct6687d {
    /// Check the family within the caller's configuration session, before
    /// touching any logical device or board-specific pin mux.
    fn require_chip(&self) -> Result<(), DeviceError> {
        let id = self.read_chip_id();
        if !Nct6687dChip::matches_chip_id(id) {
            fstart_log::error!("NCT6687D: unexpected chip ID {:#06x}", id);
            return Err(DeviceError::InitFailed);
        }
        fstart_log::debug!("NCT6687D: chip ID {:#06x}", id);
        Ok(())
    }

    /// Apply a known board's multifunction policy in source order. Caller
    /// owns the NCT config window; pin policy belongs to the board. UART PnP
    /// resources/console setup are still handled by the common SuperIo driver.
    ///
    /// Like coreboot's bootblock, write without reading back: after a power-on
    /// CR13 reads 0 right after it is written, and stopping there would leave
    /// the UART pins unrouted. The UART resources are checked afterwards
    /// (`verify_com_ports`).
    pub fn configure_pins(&mut self, config: PinConfig) -> Result<(), DeviceError> {
        self.enter_config();
        let result = (|| {
            self.require_chip()?;
            for (register, value) in [
                (0x13, config.cr13),
                (0x1d, config.cr1d),
                (0x1f, config.cr1f),
                (0x22, config.cr22),
                (0x23, config.cr23),
                (0x24, config.cr24),
                (0x25, config.cr25),
                (0x29, config.cr29),
                (0x2a, config.cr2a),
            ] {
                self.register::<()>(register).set(value);
            }
            let mut device = self.logical_device(SLEEP_POWER_LDN);
            device.write(0xf3, config.sleep_power_f3);
            Ok(())
        })();
        self.exit_config();
        result
    }

    /// Source pin-selection bytes, not fan curves or proof of tach/PWM wiring.
    /// Fan-engine policy must be validated separately before this mutation.
    pub fn configure_tach_pwm(&mut self, tach: u8, pwm: u8) -> Result<(), DeviceError> {
        self.enter_config();
        let result = (|| {
            self.require_chip()?;
            let mut device = self.logical_device(TACH_PWM_LDN);
            if self.register::<()>(0x07).get() != TACH_PWM_LDN {
                return Err(DeviceError::InitFailed);
            }
            for (offset, value) in [(0xe4, tach), (0xe5, pwm)] {
                device.write(offset, value);
                if self.register::<()>(offset).get() != value {
                    return Err(DeviceError::InitFailed);
                }
            }
            Ok(())
        })();
        self.exit_config();
        result
    }

    /// Single-base EC PnP resource, not a generic two-base env controller.
    /// This ONLY enables decode; it does not initialize sensors/fan control or
    /// certify safe thermal policy. The caller must perform that real setup.
    pub fn enable_ec_decode(&mut self, base: u16) -> Result<FirmwareEc, DeviceError> {
        let ec = FirmwareEc::ports(base).map_err(|_| DeviceError::ConfigError)?;
        self.enter_config();
        let result = (|| {
            self.require_chip()?;
            let mut device = self.logical_device(EC_LDN);
            if self.register::<()>(0x07).get() != EC_LDN {
                return Err(DeviceError::InitFailed);
            }
            device.set_enabled(false);
            device.set_io_base(IoResource::Primary(base));
            device.set_enabled(true);
            if !device.is_enabled() || !device.matches_io_base(IoResource::Primary(base)) {
                return Err(DeviceError::InitFailed);
            }
            Ok(())
        })();
        self.exit_config();
        result.map(|()| ec)
    }

    /// Allocate the source ACPI LDN's single base and apply power-fail policy
    /// without replacing neighboring CR EB bits. No IRQ is fabricated.
    pub fn enable_acpi_decode(
        &mut self,
        base: u16,
        policy: AfterPowerFailure,
    ) -> Result<(), DeviceError> {
        if base == 0 || base & !0x0ff8 != 0 {
            return Err(DeviceError::ConfigError);
        }
        self.enter_config();
        let result = (|| {
            self.require_chip()?;
            let mut device = self.logical_device(ACPI_LDN);
            if self.register::<()>(0x07).get() != ACPI_LDN {
                return Err(DeviceError::InitFailed);
            }
            device.set_enabled(false);
            device.set_io_base(IoResource::Primary(base));
            let power = self.register::<AcpiPowerLoss::Register>(0xeb);
            power.modify(AcpiPowerLoss::POLICY.val(policy as u8));
            device.set_enabled(true);
            if !device.is_enabled()
                || !device.matches_io_base(IoResource::Primary(base))
                || power.read(AcpiPowerLoss::POLICY) != policy as u8
            {
                return Err(DeviceError::InitFailed);
            }
            Ok(())
        })();
        self.exit_config();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_6687d_revisions_and_observed_m900_id_only() {
        for id in [0xd590, 0xd592, 0xd59f, 0xc73a] {
            assert!(Nct6687dChip::matches_chip_id(id));
        }
        for id in [0xc730, 0xc739, 0xc73b, 0xd440, 0xffff, 0] {
            assert!(!Nct6687dChip::matches_chip_id(id));
        }
    }

    #[test]
    fn nct6687d_rejects_incompatible_generic_ec_gpio_and_uart_resources_without_io() {
        let mut config = SuperIoConfig {
            env_controller: Some(crate::EcConfig {
                io_base: 0xa20,
                io_ext: 0,
            }),
            ..Default::default()
        };
        assert_eq!(
            Nct6687dChip::validate_config(&config),
            Err(DeviceError::ConfigError)
        );
        config.env_controller = None;
        config.gpio = Some(crate::GpioConfig { io_base: 0xa30 });
        assert_eq!(
            Nct6687dChip::validate_config(&config),
            Err(DeviceError::ConfigError)
        );
        config.gpio = None;
        for base in [0, 0x3f9, 0x13f8] {
            config.com1 = Some(crate::ComPortConfig {
                io_base: base,
                irq: 3,
                baud_rate: 115200,
            });
            assert_eq!(
                Nct6687dChip::validate_config(&config),
                Err(DeviceError::ConfigError)
            );
        }
        config.com1 = Some(crate::ComPortConfig {
            io_base: 0x3f8,
            irq: 3,
            baud_rate: 115200,
        });
        Nct6687dChip::validate_config(&config).unwrap();
    }

    #[test]
    fn nct6687d_validates_all_exposed_parallel_cir_and_keyboard_address_masks_without_io() {
        let mut config = SuperIoConfig::default();
        for base in [0, 0x379, 0x1378] {
            config.parallel = Some(crate::ParallelConfig {
                io_base: base,
                irq: 7,
            });
            assert_eq!(
                Nct6687dChip::validate_config(&config),
                Err(DeviceError::ConfigError)
            );
        }
        config.parallel = Some(crate::ParallelConfig {
            io_base: 0x378,
            irq: 7,
        });
        Nct6687dChip::validate_config(&config).unwrap();
        for base in [0, 0x2f9, 0x12f8] {
            config.cir = Some(crate::CirConfig {
                io_base: base,
                irq: 5,
            });
            assert_eq!(
                Nct6687dChip::validate_config(&config),
                Err(DeviceError::ConfigError)
            );
        }
        config.cir = Some(crate::CirConfig {
            io_base: 0x2f8,
            irq: 5,
        });
        Nct6687dChip::validate_config(&config).unwrap();
        for (base, extended) in [(0, 0x64), (0x1060, 0x64), (0x60, 0), (0x60, 0x1064)] {
            config.keyboard = Some(crate::KbcConfig {
                io_base: base,
                io_ext: extended,
                irq: 1,
                enable_keyboard_irq: true,
            });
            assert_eq!(
                Nct6687dChip::validate_config(&config),
                Err(DeviceError::ConfigError)
            );
        }
        config.keyboard = Some(crate::KbcConfig {
            io_base: 0x60,
            io_ext: 0x64,
            irq: 1,
            enable_keyboard_irq: true,
        });
        Nct6687dChip::validate_config(&config).unwrap();
    }
}
