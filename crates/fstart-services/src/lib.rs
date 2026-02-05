//! Service trait definitions.
//!
//! Services are the abstraction layer between firmware capabilities and
//! hardware drivers. Drivers implement these traits. Capabilities consume them.
//!
//! This crate defines traits only — no implementations.

#![no_std]

pub mod block;
pub mod console;
pub mod timer;

pub use block::BlockDevice;
pub use console::Console;
pub use timer::Timer;

/// Common error type for service operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceError {
    /// Operation timed out
    Timeout,
    /// Invalid parameter
    InvalidParam,
    /// Hardware error
    HardwareError,
    /// Operation not supported by this driver
    NotSupported,
    /// Device not yet initialized
    NotInitialized,
    /// Generic I/O error
    IoError,
}
