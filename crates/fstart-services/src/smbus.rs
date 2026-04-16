//! SMBus (System Management Bus) service.
//!
//! Subset of I2C at the protocol level but with distinct command
//! primitives (quick, byte, word, block). Exposed separately from
//! [`crate::i2c`] because the x86 southbridge SMBus controllers
//! (ICH/PCH) implement a dedicated SMBus transaction engine rather
//! than a generic I2C master.
//!
//! SMBus children (SPD EEPROMs, clock generators) list their 7-bit
//! slave address via [`crate::device::BusDevice`].

use crate::ServiceError;

/// Provider for SMBus transactions.
pub trait SmBus: Send + Sync {
    /// Read a single byte from `addr` at `cmd`.
    fn read_byte(&mut self, addr: u8, cmd: u8) -> Result<u8, ServiceError>;
    /// Write a single byte to `addr` at `cmd`.
    fn write_byte(&mut self, addr: u8, cmd: u8, value: u8) -> Result<(), ServiceError>;
}
