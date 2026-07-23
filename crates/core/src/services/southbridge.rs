//! Southbridge GPIO access service.
//!
//! Implemented by x86 southbridge / PCH drivers so board code can read and
//! write chipset-owned GPIO pins (dock detect, mux selects) without
//! duplicating the driver's GPIO base in board config. Hardware init
//! sequencing is not part of this trait; it lives in the fixed per-family
//! flows that call chipset driver methods directly.

use super::ServiceError;

/// Board-facing access to southbridge-owned GPIO pins.
pub trait Southbridge: Send + Sync {
    /// Read a southbridge-owned GPIO pin.
    fn gpio_get(&self, _pin: u32) -> Result<bool, ServiceError> {
        Err(ServiceError::NotSupported)
    }

    /// Set a southbridge-owned GPIO pin.
    fn gpio_set(&self, _pin: u32, _value: bool) -> Result<(), ServiceError> {
        Err(ServiceError::NotSupported)
    }
}
