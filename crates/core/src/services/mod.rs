//! Service trait definitions.
//!
//! Services are the abstraction layer between firmware stage flows and
//! hardware drivers. Drivers implement these traits; fixed stage flows consume
//! them directly.
//!
//! This module defines service contracts and small adapters, not hardware drivers.

pub mod block;
pub mod boot;
pub mod boot_media;
pub mod console;
pub mod device;
pub mod ffs_context;
pub mod firmware;
pub mod flash;
pub mod framebuffer;
pub mod i2c;
pub mod measure;
pub mod memory_detect;
pub mod memory_init;
pub mod smbus;
pub mod spi;
pub mod timer;

pub use block::BlockDevice;
pub use boot::BootLinuxParams;
pub use boot_media::{
    BlockDeviceMedia, BootMedia, MemoryMapped, SliceMedia, SubRegion, TempRamArena,
};
pub use console::{Console, ConsoleDevice, NoConsole, UartPort};
pub use device::DeviceError;
pub use firmware::{FirmwareImage, FirmwareImageProvider, FirmwareWindow, MAX_FIRMWARE_WINDOWS};
pub use flash::{FlashRegion, NorFlashAdapter, NorFlashAdapterError};
pub use framebuffer::{Framebuffer, FramebufferInfo};
pub use i2c::I2c;
pub use measure::{Measure, Measurement, Unmeasured};
pub use memory_init::MemoryInit;
pub use smbus::SmBus;
pub use spi::SpiBus;
#[cfg(any(test, feature = "testing"))]
pub use timer::ManualTimer;
pub use timer::{Timer, TimerDelay, poll_tries, poll_until};

/// Common error type for service operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ufmt::derive::uDebug)]
pub enum ServiceError {
    /// Operation timed out
    Timeout,
    /// Invalid parameter
    InvalidParam,
    /// Hardware error
    HardwareError,
    /// The addressed device is not present: it did not acknowledge
    /// (SMBus NAK). Distinct from a host-controller or bus failure, so
    /// probes can treat an absent device as a normal result.
    NoDevice,
    /// Operation not supported by this driver
    NotSupported,
    /// Device not yet initialized
    NotInitialized,
    /// Generic I/O error
    IoError,
}

impl From<DeviceError> for ServiceError {
    fn from(error: DeviceError) -> Self {
        match error {
            DeviceError::MissingResource(_) | DeviceError::ConfigError => Self::InvalidParam,
            DeviceError::InitFailed => Self::HardwareError,
            DeviceError::BusError => Self::IoError,
        }
    }
}

impl embedded_io::Error for ServiceError {
    fn kind(&self) -> embedded_io::ErrorKind {
        match self {
            Self::Timeout => embedded_io::ErrorKind::TimedOut,
            Self::InvalidParam => embedded_io::ErrorKind::InvalidInput,
            Self::NotSupported => embedded_io::ErrorKind::Unsupported,
            Self::NoDevice => embedded_io::ErrorKind::NotFound,
            Self::NotInitialized => embedded_io::ErrorKind::NotConnected,
            Self::HardwareError | Self::IoError => embedded_io::ErrorKind::Other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{FirmwareImage, FirmwareWindow, ServiceError, ffs_context};

    #[test]
    fn firmware_image_validation_rejects_bad_window_count() {
        let image = FirmwareImage {
            size: 0x1000,
            windows: [FirmwareWindow::EMPTY; super::MAX_FIRMWARE_WINDOWS],
            window_count: (super::MAX_FIRMWARE_WINDOWS + 1) as u8,
        };

        assert_eq!(image.validate(), Err(ServiceError::InvalidParam));
        assert_eq!(image.active_windows().len(), super::MAX_FIRMWARE_WINDOWS);
    }

    #[test]
    fn firmware_image_validation_rejects_overflow_and_out_of_bounds_windows() {
        let mut windows = [FirmwareWindow::EMPTY; super::MAX_FIRMWARE_WINDOWS];
        windows[0] = FirmwareWindow::new(0x800, 0x1000, 0x900);
        let out_of_bounds = FirmwareImage {
            size: 0x1000,
            windows,
            window_count: 1,
        };
        assert_eq!(out_of_bounds.validate(), Err(ServiceError::InvalidParam));

        let mut windows = [FirmwareWindow::EMPTY; super::MAX_FIRMWARE_WINDOWS];
        windows[0] = FirmwareWindow::new(u64::MAX - 1, 0x1000, 4);
        let overflow = FirmwareImage {
            size: u64::MAX,
            windows,
            window_count: 1,
        };
        assert_eq!(overflow.validate(), Err(ServiceError::InvalidParam));
    }

    #[test]
    fn multi_window_memory_mapped_reads_maps_and_checks_overlap() {
        let win0 = [0x11u8, 0x22, 0x33, 0x44];
        let win1 = [0xaau8, 0xbb, 0xcc, 0xdd];
        let mut image = FirmwareImage::EMPTY;
        image.size = 8;
        image.windows[0] = FirmwareWindow::new(0, win0.as_ptr() as u64, 4);
        image.windows[1] = FirmwareWindow::new(4, win1.as_ptr() as u64, 4);
        image.window_count = 2;

        // SAFETY: both slices stay alive for the test.
        let media = unsafe { super::MemoryMapped::from_image(image) }.unwrap();
        assert_eq!(super::BootMedia::size(&media), 8);
        assert_eq!(
            super::BootMedia::map(&media, 1, 3),
            Some(&[0x22, 0x33, 0x44][..])
        );
        assert_eq!(super::BootMedia::map(&media, 4, 2), Some(&[0xaa, 0xbb][..]));
        // Cross-window range cannot be mapped as a single slice, but read_at spans both.
        assert_eq!(super::BootMedia::map(&media, 2, 4), None);
        let mut buf = [0u8; 4];
        assert_eq!(super::BootMedia::read_at(&media, 2, &mut buf), Ok(4));
        assert_eq!(buf, [0x33, 0x44, 0xaa, 0xbb]);

        assert!(super::BootMedia::overlaps_range(
            &media,
            win0.as_ptr() as u64 + 2,
            4
        ));
        assert!(super::BootMedia::overlaps_range(
            &media,
            win1.as_ptr() as u64,
            1
        ));
    }

    #[test]
    fn ffs_context_does_not_publish_null_base_window() {
        static ANCHOR: [u8; 8] = [0; 8];

        ffs_context::set_memory_mapped(&ANCHOR, 0, 0x1000);

        assert!(ffs_context::memory_mapped().is_none());
    }
}
