//! Console service — serial/UART abstraction.

use super::ServiceError;

/// Statically constructed console used by fixed platform flows.
///
/// The associated configuration remains plain serializable board data while
/// the concrete console type is selected at compile time.
pub trait ConsoleDevice: Console + Sized {
    /// Plain configuration used to construct this console.
    type Config;

    /// Human-readable implementation name for diagnostics.
    const NAME: &'static str;

    /// Construct a console without initializing hardware.
    fn new(config: Self::Config) -> Result<Self, ServiceError>;

    /// Initialize the console hardware.
    fn init(&mut self) -> Result<(), ServiceError>;

    /// Where the console UART sits, for payloads that keep using it.
    fn uart_port(&self) -> Option<UartPort> {
        None
    }
}

/// A console UART as payloads and OSes need to find it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UartPort {
    /// I/O port when [`io_port`](Self::io_port), else MMIO address.
    pub base: u64,
    pub io_port: bool,
    /// Bytes between registers.
    pub reg_stride: u32,
    pub baud: u32,
    /// UART input clock.
    pub clock_hz: u32,
}

/// A console device for debug output and (optionally) input.
pub trait Console: Send + Sync {
    /// Write a single byte.
    fn write_byte(&self, byte: u8) -> Result<(), ServiceError>;

    /// Read a single byte (non-blocking). Returns `Ok(None)` if no data available.
    fn read_byte(&self) -> Result<Option<u8>, ServiceError>;

    /// Write a byte slice.
    fn write_bytes(&self, bytes: &[u8]) -> Result<(), ServiceError> {
        for &b in bytes {
            self.write_byte(b)?;
        }
        Ok(())
    }

    /// Write a string.
    fn write_str(&self, s: &str) -> Result<(), ServiceError> {
        self.write_bytes(s.as_bytes())
    }

    /// Wait until all queued/transmitting console bytes have drained.
    fn flush(&self) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Write a string followed by a newline.
    fn write_line(&self, s: &str) -> Result<(), ServiceError> {
        self.write_str(s)?;
        self.write_byte(b'\n')?;
        self.flush()
    }
}
