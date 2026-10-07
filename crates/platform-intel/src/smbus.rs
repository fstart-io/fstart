//! Board SMBus routing policy used by the fixed Intel flow.

use fstart_core::services::ServiceError;

#[derive(Clone, Copy)]
pub enum SmbusRoute {
    Spd,
    Eeprom,
}

/// Directly connected buses need no override. A muxed board supplies only
/// selection policy; the existing southbridge remains the SMBus provider.
pub trait IntelSmbusRouting<S> {
    fn select_smbus(_southbridge: &S, _route: SmbusRoute) -> Result<(), ServiceError> {
        Ok(())
    }

    /// Run a board device operation on the SPD branch, then explicitly restore
    /// EEPROM routing even on transfer failure. Report restoration failures.
    fn with_spd<T>(
        southbridge: &mut S,
        operation: impl FnOnce(&mut S) -> Result<T, ServiceError>,
    ) -> Result<T, ServiceError> {
        Self::select_smbus(southbridge, SmbusRoute::Spd)?;
        let result = operation(southbridge);
        Self::select_smbus(southbridge, SmbusRoute::Eeprom)?;
        result
    }
}
