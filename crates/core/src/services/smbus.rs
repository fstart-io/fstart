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

use super::ServiceError;

/// Provider for SMBus transactions.
///
/// Mirrors the coreboot SMBus protocol stack: quick, byte, word, and block.
pub trait SmBus: Send + Sync {
    /// Read a single byte from `addr` at `cmd`.
    fn read_byte(&mut self, addr: u8, cmd: u8) -> Result<u8, ServiceError>;
    /// Write a single byte to `addr` at `cmd`.
    fn write_byte(&mut self, addr: u8, cmd: u8, value: u8) -> Result<(), ServiceError>;
    /// Read a 16-bit word from `addr` at `cmd` (low byte first).
    fn read_word(&mut self, addr: u8, cmd: u8) -> Result<u16, ServiceError> {
        let _ = (addr, cmd);
        Err(ServiceError::HardwareError)
    }
    /// Write a 16-bit word to `addr` at `cmd` (low byte first).
    fn write_word(&mut self, addr: u8, cmd: u8, value: u16) -> Result<(), ServiceError> {
        let _ = (addr, cmd, value);
        Err(ServiceError::HardwareError)
    }
    /// Block read: read up to `buf.len()` bytes. Returns bytes read.
    fn block_read(&mut self, addr: u8, cmd: u8, buf: &mut [u8]) -> Result<usize, ServiceError> {
        let _ = (addr, cmd, buf);
        Err(ServiceError::HardwareError)
    }
    /// Block write: write `data` bytes to `addr` at `cmd`.
    fn block_write(&mut self, addr: u8, cmd: u8, data: &[u8]) -> Result<(), ServiceError> {
        let _ = (addr, cmd, data);
        Err(ServiceError::HardwareError)
    }
    /// Fill `buf` from an I2C EEPROM (such as an SPD) starting at `offset`.
    ///
    /// Controllers with an I2C block read override this with one sequential
    /// read; the default falls back to one byte-data read per byte. A device
    /// that does not answer reports [`ServiceError::NoDevice`].
    fn i2c_eeprom_read(
        &mut self,
        addr: u8,
        offset: u8,
        buf: &mut [u8],
    ) -> Result<(), ServiceError> {
        if usize::from(offset) + buf.len() > 256 {
            return Err(ServiceError::InvalidParam);
        }
        buf.iter_mut()
            .zip(offset..=u8::MAX)
            .try_for_each(|(byte, cmd)| {
                *byte = self.read_byte(addr, cmd)?;
                Ok(())
            })
    }
}
