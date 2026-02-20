//! Service trait definitions.
//!
//! Services are the abstraction layer between firmware capabilities and
//! hardware drivers. Drivers implement these traits. Capabilities consume them.
//!
//! This crate defines traits only — no implementations. It also defines the
//! `Device` trait that all drivers implement for lifecycle management.
//!
//! See [docs/driver-model.md](../../docs/driver-model.md) for the full
//! driver model architecture.

#![no_std]

pub mod block;
pub mod boot_media;
pub mod console;
pub mod device;
pub mod gpio;
pub mod i2c;
pub mod spi;
pub mod timer;

pub use block::BlockDevice;
pub use boot_media::{BlockDeviceMedia, BootMedia, FlashMap, LinearMap, MemoryMapped, SubRegion};
pub use console::Console;
pub use device::{BusDevice, Device, DeviceError};
pub use gpio::GpioController;
pub use i2c::I2c;
pub use spi::SpiBus;
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
