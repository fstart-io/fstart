//! GPIO controller service — general-purpose I/O abstraction.

use crate::ServiceError;

/// A GPIO controller that manages general-purpose I/O pins.
///
/// Implemented by GPIO controller drivers. Provides basic pin
/// read/write/direction control.
pub trait GpioController: Send + Sync {
    /// Read the current value of a GPIO pin.
    ///
    /// Returns `true` for high, `false` for low.
    fn get(&self, pin: u32) -> Result<bool, ServiceError>;

    /// Set the output value of a GPIO pin.
    ///
    /// `value`: `true` for high, `false` for low.
    fn set(&self, pin: u32, value: bool) -> Result<(), ServiceError>;

    /// Set the direction of a GPIO pin.
    ///
    /// `output`: `true` for output mode, `false` for input mode.
    fn set_direction(&self, pin: u32, output: bool) -> Result<(), ServiceError>;
}
