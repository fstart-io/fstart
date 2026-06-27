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

#[cfg(test)]
extern crate std;

pub mod acpi_provider;
pub mod block;
pub mod boot;
pub mod boot_media;
pub mod clock;
pub mod console;
pub mod device;
pub mod ffs_context;
pub mod firmware;
pub mod flash_layout;
pub mod framebuffer;
pub mod gpio;
pub mod i2c;
pub mod init;
pub mod lpc;
pub mod mainboard;
pub mod memory_controller;
pub mod memory_detect;
pub mod network;
#[cfg(feature = "pci")]
pub mod pci;
pub mod pci_host;
pub mod smbus;
pub mod soc_boot;
pub mod southbridge;
pub mod spi;
pub mod timer;

pub use block::BlockDevice;
pub use boot::BootLinuxParams;
pub use boot_media::{
    BlockDeviceMedia, BootMedia, FirmwareImageMap, FlashMap, LinearMap, MemoryMapped, SubRegion,
    TempRamArena,
};
pub use clock::ClockController;
pub use console::Console;
pub use device::{BusDevice, Device, DeviceError};
pub use firmware::{FirmwareImage, FirmwareImageProvider, FirmwareWindow};
pub use flash_layout::FlashLayoutVerifier;
pub use framebuffer::{Framebuffer, FramebufferInfo};
pub use gpio::GpioController;
pub use i2c::I2c;
pub use init::{
    EarlyInit, FinalizeInit, HardwareInit, InitContext, PostDramInit, PreConsoleInit,
    StageLocalInit,
};
pub use lpc::LpcBus;
pub use mainboard::Mainboard;
pub use memory_controller::MemoryController;
pub use network::Network;
#[cfg(feature = "pci")]
pub use pci::{PciBdf, PciRootBus, PciWindow, PciWindowKind};
pub use pci_host::PciHost;
pub use smbus::SmBus;
pub use soc_boot::SocBootHeader;
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

#[cfg(test)]
mod tests {
    use crate::{
        ffs_context, FirmwareImage, FirmwareImageMap, FirmwareWindow, FlashMap, ServiceError,
    };

    #[test]
    fn firmware_image_validation_rejects_bad_window_count() {
        let image = FirmwareImage {
            size: 0x1000,
            windows: [FirmwareWindow::EMPTY; 4],
            window_count: 5,
        };

        assert_eq!(image.validate(), Err(ServiceError::InvalidParam));
        assert_eq!(image.active_windows().len(), 4);
    }

    #[test]
    fn firmware_image_validation_rejects_overflow_and_out_of_bounds_windows() {
        let out_of_bounds = FirmwareImage {
            size: 0x1000,
            windows: [
                FirmwareWindow::new(0x800, 0x1000, 0x900),
                FirmwareWindow::EMPTY,
                FirmwareWindow::EMPTY,
                FirmwareWindow::EMPTY,
            ],
            window_count: 1,
        };
        assert_eq!(out_of_bounds.validate(), Err(ServiceError::InvalidParam));

        let overflow = FirmwareImage {
            size: u64::MAX,
            windows: [
                FirmwareWindow::new(u64::MAX - 1, 0x1000, 4),
                FirmwareWindow::EMPTY,
                FirmwareWindow::EMPTY,
                FirmwareWindow::EMPTY,
            ],
            window_count: 1,
        };
        assert_eq!(overflow.validate(), Err(ServiceError::InvalidParam));
    }

    #[test]
    fn firmware_image_map_does_not_expose_null_base_slice() {
        let map = FirmwareImageMap::new(FirmwareImage::single_window(0, 0x1000));

        assert!(map.as_contiguous_slice(0x1000).is_none());
        assert_eq!(map.contiguous_len(0, 0x100), 0x100);
    }

    #[test]
    fn ffs_context_does_not_publish_null_base_window() {
        static ANCHOR: [u8; 8] = [0; 8];

        ffs_context::set_memory_mapped(&ANCHOR, 0, 0x1000);

        assert!(ffs_context::memory_mapped().is_none());
    }
}
