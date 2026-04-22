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

pub mod acpi_provider;
pub mod block;
pub mod boot;
pub mod boot_media;
pub mod clock;
pub mod console;
pub mod device;
pub mod framebuffer;
pub mod gpio;
pub mod i2c;
pub mod lpc;
pub mod memory_controller;
pub mod memory_detect;
pub mod network;
pub mod pci;
pub mod pci_host;
pub mod smbus;
pub mod soc_boot;
pub mod southbridge;
pub mod spi;
pub mod timer;

pub use block::BlockDevice;
pub use boot_media::{BlockDeviceMedia, BootMedia, FlashMap, LinearMap, MemoryMapped, SubRegion};
pub use clock::ClockController;
pub use console::Console;
pub use device::{BusDevice, Device, DeviceError};
pub use framebuffer::{Framebuffer, FramebufferInfo};
pub use gpio::GpioController;
pub use i2c::I2c;
pub use lpc::LpcBus;
pub use memory_controller::MemoryController;
pub use network::Network;
pub use pci::{PciAddr, PciRootBus, PciWindow, PciWindowKind};
pub use pci_host::PciHost;
pub use smbus::SmBus;
pub use southbridge::Southbridge;
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
