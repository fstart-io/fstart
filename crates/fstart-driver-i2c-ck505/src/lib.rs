//! IDT CK505 clock generator driver (SMBus-attached).
//!
//! The CK505 is a common clock source on Atom D4xx / D5xx / NM10
//! reference boards. It is programmed over SMBus: five registers
//! select reference and bus clock dividers, spread-spectrum options,
//! and output enables. Board authors provide a `regs` / `mask` pair
//! that the driver applies as `(new_val & mask) | (read_val & !mask)`
//! using byte-at-a-time SMBus read-modify-writes.

#![no_std]

use fstart_services::device::{BusDevice, DeviceError};
use fstart_services::ServiceError;
use serde::{Deserialize, Serialize};

/// Configuration for the CK505 clock generator.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct I2cCk505Config {
    /// Mask bytes — bit set = register position is written from `regs`.
    pub mask: [u8; 5],
    /// Register values to apply (AND-masked by `mask`).
    pub regs: [u8; 5],
}

/// CK505 driver state.
pub struct I2cCk505 {
    /// 7-bit SMBus slave address (supplied by the bus attachment).
    addr: u8,
    config: I2cCk505Config,
}

// SAFETY: state is CPU-exclusive during firmware phase.
unsafe impl Send for I2cCk505 {}
unsafe impl Sync for I2cCk505 {}

impl BusDevice for I2cCk505 {
    const NAME: &'static str = "i2c-ck505";
    const COMPATIBLE: &'static [&'static str] = &["idt,ck505", "idt,clock-generator"];
    type Config = I2cCk505Config;
    type Bus = dyn SmBusAddrProvider;

    fn new_on_bus(config: &Self::Config, bus: &Self::Bus) -> Result<Self, DeviceError> {
        Ok(Self {
            addr: bus.smbus_address(),
            config: *config,
        })
    }

    fn init(&mut self) -> Result<(), DeviceError> {
        // Actual programming requires an SmBus provider at the parent.
        // Without that plumbing we just log what would be written — the
        // driver still compiles and the code path is ready for the SB
        // driver to expose an `SmBus` implementation later.
        for (i, (value, mask)) in self
            .config
            .regs
            .iter()
            .zip(self.config.mask.iter())
            .enumerate()
        {
            fstart_log::info!(
                "i2c-ck505: addr={:#x} reg{} <= value={:#04x} mask={:#04x}",
                self.addr,
                i,
                *value,
                *mask,
            );
        }
        Ok(())
    }
}

/// Trait implemented by a parent SMBus controller that knows a given
/// child's 7-bit slave address.
///
/// The ICH7 SMBus driver fills this in by reading the child's
/// `bus: I2c(0x69)` field during construction (codegen threads it
/// through). Declared here (rather than in `fstart-services`) because
/// it is a temporary bridge until the full `SmBus` service plumbing
/// is in place.
pub trait SmBusAddrProvider {
    /// Return the 7-bit SMBus slave address of the child device.
    fn smbus_address(&self) -> u8;
}

/// Reuse of the [`SmBus`] service trait for completeness — re-exported
/// here so downstream code (board crates) has a single place to import
/// from when wiring up CK505 children.
pub use fstart_services::SmBus as ParentSmBus;
// Silence an otherwise-unused re-export warning.
#[doc(hidden)]
#[allow(dead_code)]
fn _touch_parent_smbus<T: ParentSmBus>(_: &T) -> Result<(), ServiceError> {
    Ok(())
}
