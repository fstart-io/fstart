//! I2C bus service — I2C controller abstraction.

use crate::ServiceError;

/// An I2C bus controller that can perform transactions to child addresses.
///
/// Implemented by I2C controller drivers (e.g., DesignWare APB I2C).
/// Child devices on the bus use this trait via their parent reference
/// to communicate with their hardware.
pub trait I2cBus: Send + Sync {
    /// Read `buf.len()` bytes from `reg` on device at `addr`.
    ///
    /// Returns the number of bytes actually read.
    fn read(&self, addr: u8, reg: u8, buf: &mut [u8]) -> Result<usize, ServiceError>;

    /// Write `data` to `reg` on device at `addr`.
    ///
    /// Returns the number of bytes actually written.
    fn write(&self, addr: u8, reg: u8, data: &[u8]) -> Result<usize, ServiceError>;
}
