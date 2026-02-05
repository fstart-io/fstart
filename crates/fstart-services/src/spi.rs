//! SPI bus service — SPI controller abstraction.

use crate::ServiceError;

/// An SPI bus controller that can perform transactions via chip-select lines.
///
/// Implemented by SPI controller drivers. Child devices select their
/// chip-select index; the controller handles the physical signalling.
pub trait SpiBus: Send + Sync {
    /// Perform a simultaneous transmit/receive SPI transfer.
    ///
    /// Selects chip-select `cs`, clocks out `tx` while clocking in `rx`.
    /// `tx` and `rx` must have the same length.
    /// Returns the number of bytes transferred.
    fn transfer(&self, cs: u8, tx: &[u8], rx: &mut [u8]) -> Result<usize, ServiceError>;

    /// Write-only SPI transfer (discard received data).
    fn write(&self, cs: u8, data: &[u8]) -> Result<usize, ServiceError> {
        let mut dummy = [0u8; 0];
        // Default implementation: transfer with empty rx.
        // Drivers should override for efficiency.
        self.transfer(cs, data, &mut dummy)
    }

    /// Read-only SPI transfer (send zeros).
    fn read(&self, cs: u8, buf: &mut [u8]) -> Result<usize, ServiceError> {
        let zeros = [0u8; 0];
        // Default implementation: transfer with empty tx.
        // Drivers should override for efficiency.
        self.transfer(cs, &zeros, buf)
    }
}
