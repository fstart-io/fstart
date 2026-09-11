//! Allocation-free geometry adapter for descriptor-backed Intel stages.
//!
//! These bounds accompany the fixed authenticated bootstrap flow. They neither
//! authenticate an incoming executable nor establish that DRAM was trained.

use fstart_core::layout::{Layout, Region, RegionKind};
use fstart_core::services::ServiceError;

#[derive(Clone, Copy)]
pub struct IntelBootLayout<'a> {
    layout: Layout<'a>,
}

impl<'a> IntelBootLayout<'a> {
    pub fn parse(layout: Layout<'a>, expected_stage: u16) -> Result<Self, ServiceError> {
        if expected_stage > 2 || layout.stage_index() != expected_stage {
            return Err(ServiceError::InvalidParam);
        }
        let this = Self { layout };
        for kind in [
            RegionKind::Image,
            RegionKind::Writable,
            RegionKind::Stack,
            RegionKind::Flash,
            RegionKind::Firmware,
            RegionKind::BootstrapRam,
            RegionKind::BootstrapPostcar,
            RegionKind::BootstrapMainstage,
            RegionKind::BootMediaScratch,
        ] {
            this.region(kind)?;
        }
        let ram = this.region(RegionKind::BootstrapRam)?;
        let end = ram.end().ok_or(ServiceError::InvalidParam)?;
        this.destination(RegionKind::BootstrapPostcar, end)?;
        this.destination(RegionKind::BootstrapMainstage, end)?;
        Ok(this)
    }

    /// Exactly one region must describe each scalar role. Repeated exclusions
    /// are exposed separately rather than silently taking the first one.
    pub fn region(&self, kind: RegionKind) -> Result<Region, ServiceError> {
        let mut matches = self.layout.regions().filter(|region| region.kind == kind);
        let region = matches.next().ok_or(ServiceError::InvalidParam)?;
        if matches.next().is_some() {
            return Err(ServiceError::InvalidParam);
        }
        Ok(region)
    }

    pub fn exclusions(&self) -> impl Iterator<Item = Region> + '_ {
        self.layout
            .regions()
            .filter(|region| region.kind == RegionKind::Reserved)
    }

    /// Check the whole future footprint, not merely the bytes being copied.
    /// Do not truncate a reservation to detected RAM: its future stack or heap
    /// could then sit outside trained memory even if initialized output fits.
    pub fn destination(
        &self,
        role: RegionKind,
        trained_ram_end: u64,
    ) -> Result<Region, ServiceError> {
        if !matches!(
            role,
            RegionKind::BootstrapPostcar | RegionKind::BootstrapMainstage
        ) {
            return Err(ServiceError::InvalidParam);
        }
        bounded_destination(
            self.region(role)?,
            self.region(RegionKind::BootstrapRam)?,
            trained_ram_end,
        )
    }
}

/// Linked stage geometry for fixed Intel flows. All Intel boards resolve
/// their windows from the linked runtime descriptor; there is no authored
/// board geometry fallback.
#[cfg(feature = "stage")]
impl IntelBootLayout<'static> {
    pub fn firmware(self) -> Result<(u64, usize), ServiceError> {
        let region = self.region(RegionKind::Firmware)?;
        Ok((
            region.base,
            region
                .size
                .try_into()
                .map_err(|_| ServiceError::InvalidParam)?,
        ))
    }

    pub fn current(expected_stage: u16) -> Result<Self, ServiceError> {
        Self::parse(
            fstart_stage::layout::current().map_err(|_| ServiceError::InvalidParam)?,
            expected_stage,
        )
    }
}

fn bounded_destination(
    window: Region,
    ram: Region,
    trained_ram_end: u64,
) -> Result<Region, ServiceError> {
    let end = window.end().ok_or(ServiceError::InvalidParam)?;
    let limit = ram
        .end()
        .ok_or(ServiceError::InvalidParam)?
        .min(trained_ram_end);
    if window.size == 0 || ram.size == 0 || window.base < ram.base || end > limit {
        return Err(ServiceError::InvalidParam);
    }
    Ok(window)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_successor_must_fit_both_profile_and_trained_ram() {
        let window = Region {
            kind: RegionKind::BootstrapMainstage,
            base: 0x4000000,
            size: 0x1000000,
        };
        let ram = Region {
            kind: RegionKind::BootstrapRam,
            base: 0x100000,
            size: 0x3ff00000,
        };
        assert!(bounded_destination(window, ram, 0x5000000).is_ok());
        // Initialized code could fit below 0x4800000; its future stack cannot.
        assert!(bounded_destination(window, ram, 0x4800000).is_err());
        let short_profile = Region {
            size: 0x4700000,
            ..ram
        };
        assert!(bounded_destination(window, short_profile, 0x40000000).is_err());
        assert!(
            bounded_destination(
                Region {
                    base: u64::MAX - 4095,
                    ..window
                },
                ram,
                u64::MAX
            )
            .is_err()
        );
        assert!(bounded_destination(Region { size: 0, ..window }, ram, u64::MAX).is_err());
    }
}
