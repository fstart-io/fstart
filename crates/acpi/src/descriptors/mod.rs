//! ACPI resource descriptors not provided by `acpi_tables`.
//!
//! These are Large Resource Data Type descriptors used inside
//! `ResourceTemplate` objects.  They implement the [`Aml`] trait
//! and can be passed alongside `Memory32Fixed`, `Interrupt`, etc.
//!
//! - [`gpio`] -- GPIO Connection Descriptors (GpioIo, GpioInt)
//! - [`i2c`] -- I2C Serial Bus Connection Descriptor
//! - [`spi`] -- SPI Serial Bus Connection Descriptor
//!
//! These are essential for SoC platforms where GPIO controllers,
//! I2C buses, and SPI buses expose devices to the OS via ACPI.
//!
//! [`Aml`]: acpi_tables::Aml

pub mod gpio;
pub mod i2c;
pub mod spi;
