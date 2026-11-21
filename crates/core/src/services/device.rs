//! Device construction and initialization errors.
//!
//! Drivers expose inherent constructors (e.g. `new`, `new_at_base`,
//! `new_at_address`) and `init` methods returning [`DeviceError`]; fixed
//! platform and board flows construct and initialize them directly.

/// Error type for device construction and initialisation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceError {
    /// A required resource was not provided in the board config.
    MissingResource(&'static str),
    /// The device configuration is invalid.
    ConfigError,
    /// Hardware did not respond as expected during init.
    InitFailed,
    /// A bus error occurred communicating with a parent bus.
    BusError,
}

impl ufmt::uDebug for DeviceError {
    fn fmt<W: ufmt::uWrite + ?Sized>(
        &self,
        f: &mut ufmt::Formatter<'_, W>,
    ) -> Result<(), W::Error> {
        match self {
            Self::MissingResource(name) => {
                f.write_str("MissingResource(")?;
                f.write_str(name)?;
                f.write_str(")")
            }
            Self::ConfigError => f.write_str("ConfigError"),
            Self::InitFailed => f.write_str("InitFailed"),
            Self::BusError => f.write_str("BusError"),
        }
    }
}
