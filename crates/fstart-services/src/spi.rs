//! SPI bus service — embedded-hal SPI trait re-exports.
//!
//! fstart uses [`embedded_hal::spi::SpiBus`] as the standard interface for
//! SPI bus controllers, giving access to the embedded Rust driver ecosystem.
//!
//! SPI controller drivers implement `SpiBus` (and `ErrorType`).
//! The [`SpiDevice`] trait is available for chip-select-aware wrappers.

pub use embedded_hal::spi::{
    Error, ErrorKind, ErrorType, Mode, Operation, Phase, Polarity, SpiBus, SpiDevice,
};
