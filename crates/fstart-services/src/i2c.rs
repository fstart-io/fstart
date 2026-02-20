//! I2C bus service — embedded-hal I2C trait re-exports.
//!
//! fstart uses [`embedded_hal::i2c::I2c`] as the standard interface for
//! I2C bus controllers, giving access to the embedded Rust driver ecosystem.
//!
//! I2C controller drivers implement `I2c` (and `ErrorType`). Child devices
//! on the bus use the `I2c` trait via their parent controller reference.

pub use embedded_hal::i2c::{
    Error, ErrorKind, ErrorType, I2c, NoAcknowledgeSource, Operation, SevenBitAddress,
    TenBitAddress,
};
