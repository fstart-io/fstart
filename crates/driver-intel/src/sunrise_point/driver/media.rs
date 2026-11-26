// SPDX-License-Identifier: MIT OR Apache-2.0
//! The boot flash's BIOS window, from the SPI controller's descriptor.
use super::*;
use fstart_core::services::{FirmwareImage, FirmwareImageProvider};

impl IntelSunrisePoint {
    pub(super) fn boot_flash(&self) -> Result<spi::Observation, ServiceError> {
        if self.failure.is_some()
            || !matches!(
                self.phase,
                Phase::ColdReady
                    | Phase::LateImported
                    | Phase::Devices
                    | Phase::Enumerated
                    | Phase::Initialized
            )
        {
            return Err(ServiceError::NotInitialized);
        }
        self.family().map_err(|_| ServiceError::HardwareError)?;
        spi::Observation::read(&self.spi).map_err(|_| ServiceError::HardwareError)
    }
}
impl FirmwareImageProvider for IntelSunrisePoint {
    fn firmware_image(&self) -> Result<FirmwareImage, ServiceError> {
        Ok(self.boot_flash()?.image())
    }
}
