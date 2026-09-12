//! Runtime firmware flash-layout verification service.
//!
//! Board metadata describes the expected firmware image layout at build time.  Some
//! platforms can also read the active flash/descriptor map from hardware at
//! runtime (for example Intel SPI controllers with an IFD).  Drivers implement
//! this trait to compare those two sources of truth before trusting the boot
//! medium layout.

use crate::memory::FlashLayout;

use super::ServiceError;

/// Verify that the runtime flash layout matches the build-time board config.
pub trait FlashLayoutVerifier: Send + Sync {
    /// Compare hardware-observed flash layout against `expected`.
    fn verify_flash_layout(&self, expected: &FlashLayout) -> Result<(), ServiceError>;
}
