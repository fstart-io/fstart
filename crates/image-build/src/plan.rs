//! Host plan transport, shared by platform calculation and build orchestration.
use fstart_core::layout::{Region, RegionKind};
use fstart_core::{
    ConstVec, IntelIfdFlashLayout, IntelIfdRegion, IntelIfdRegionConfig, SecurityConfig, SmmConfig,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Span {
    pub base: u64,
    pub size: u64,
}
impl Span {
    pub fn end(self) -> Result<u64, String> {
        if self.size == 0 {
            return Err("empty layout range".into());
        }
        self.base
            .checked_add(self.size)
            .ok_or_else(|| "layout range overflows".into())
    }
    pub fn contains(self, base: u64, size: u64) -> bool {
        base >= self.base
            && base
                .checked_add(size)
                .zip(self.base.checked_add(self.size))
                .is_some_and(|(end, limit)| end <= limit)
    }
    pub fn overlaps(self, other: Self) -> bool {
        self.base < other.base + other.size && other.base < self.base + self.size
    }
    pub fn region(self, kind: RegionKind) -> Region {
        Region {
            kind,
            base: self.base,
            size: self.size,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn imported_ifd_rejects_overlap_duplicate_overflow_and_bad_alignment() {
        let valid = || {
            IfdTransport(vec![
                (IntelIfdRegion::Descriptor, 0, 0x1000),
                (IntelIfdRegion::Gbe, 0x1000, 0x2000),
                (IntelIfdRegion::Me, 0x3000, 0x27d000),
                (IntelIfdRegion::Bios, 0x280000, 0x180000),
            ])
        };
        let layout = valid().decode().unwrap();
        assert_eq!(layout.size(), 0x400000);
        assert_eq!(layout.regions.len(), 4);
        assert_eq!(
            layout
                .regions
                .as_slice()
                .iter()
                .map(|r| r.kind)
                .collect::<Vec<_>>(),
            vec![
                IntelIfdRegion::Descriptor,
                IntelIfdRegion::Gbe,
                IntelIfdRegion::Me,
                IntelIfdRegion::Bios
            ]
        );
        assert_eq!(IfdTransport::from(layout).0, valid().0);
        let mut bad = valid();
        bad.0[0].1 = 0x1000;
        assert!(bad.decode().unwrap_err().contains("overlap"));
        let mut bad = valid();
        bad.0.push(bad.0[0]);
        assert!(bad.decode().unwrap_err().contains("duplicate"));
        let mut bad = valid();
        bad.0[1].1 = 0xfffff000;
        assert!(bad.decode().is_err());
        let mut bad = valid();
        bad.0[1].1 += 1;
        assert!(bad.decode().is_err());
    }
}

/// Explicit invocation choices. Defaults and supported combinations belong to
/// the selected platform, not the adapter or fbuild's family backend.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct BuildSelection {
    pub payload: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntelStagePlan {
    pub role: crate::intel_plan::IntelStage,
    /// Selected features relative to the platform dependency (qualified by Cargo's actual alias in fbuild).
    pub features: Vec<String>,
    pub payload: String,
}

/// A compiled platform export, not a board-authoring schema.
pub type ResolvedPlan = crate::build_plan::BuildPlan;

#[derive(Debug, Serialize, Deserialize)]
pub struct IntelPlan {
    pub reservations: crate::intel_plan::IntelReservations,
    pub target: String,
    pub payload: String,
    pub stages: [IntelStagePlan; 3],
    pub smm_features: Vec<String>,
    pub flash: FlashTransport,
    pub max_cpus: u16,
    pub microcode: Vec<String>,
    pub security: SecurityConfig,
    pub smm: SmmConfig,
}

/// Bounded active-region transport; authoring uses core's const-validated type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IfdTransport(pub Vec<(IntelIfdRegion, u32, u32)>);
impl From<IntelIfdFlashLayout> for IfdTransport {
    fn from(layout: IntelIfdFlashLayout) -> Self {
        Self(
            layout
                .regions
                .as_slice()
                .iter()
                .map(|r| (r.kind, r.offset, r.size))
                .collect(),
        )
    }
}
impl IfdTransport {
    pub fn decode(&self) -> Result<IntelIfdFlashLayout, String> {
        if self.0.is_empty() || self.0.len() > 8 {
            return Err("IFD needs 1..8 regions".into());
        }
        let mut regions = ConstVec::new(IntelIfdRegionConfig {
            kind: IntelIfdRegion::Descriptor,
            offset: 0,
            size: 0,
        });
        for &(kind, offset, size) in &self.0 {
            regions = regions.push(IntelIfdRegionConfig { kind, offset, size });
        }
        let layout = IntelIfdFlashLayout { regions };
        layout.check().map_err(str::to_owned)?;
        Ok(layout)
    }
}

/// Physical flash identity, without inventing descriptor regions on legacy hardware.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FlashTransport {
    IntelIfd(IfdTransport),
    X86Legacy(fstart_core::X86LegacyFlashLayout),
}

impl From<fstart_core::FlashLayout> for FlashTransport {
    fn from(layout: fstart_core::FlashLayout) -> Self {
        match layout {
            fstart_core::FlashLayout::IntelIfd(ifd) => Self::IntelIfd(ifd.into()),
            fstart_core::FlashLayout::X86Legacy(legacy) => Self::X86Legacy(legacy),
        }
    }
}

impl FlashTransport {
    pub fn decode(&self) -> Result<fstart_core::FlashLayout, String> {
        match self {
            Self::IntelIfd(ifd) => ifd.decode().map(fstart_core::FlashLayout::IntelIfd),
            Self::X86Legacy(legacy) if legacy.size.is_power_of_two() => {
                Ok(fstart_core::FlashLayout::X86Legacy(*legacy))
            }
            Self::X86Legacy(_) => {
                Err("legacy flash capacity must be a nonzero power of two".into())
            }
        }
    }

    pub fn windows(&self) -> Result<(Span, Span), String> {
        match self.decode()? {
            fstart_core::FlashLayout::IntelIfd(ifd) => Ok((
                Span {
                    base: ifd.base(),
                    size: u64::from(ifd.size()),
                },
                Span {
                    base: ifd.bios_base().ok_or("missing BIOS")?,
                    size: u64::from(ifd.bios_region().ok_or("missing BIOS")?.size),
                },
            )),
            fstart_core::FlashLayout::X86Legacy(legacy) => {
                let window = Span {
                    base: legacy.base(),
                    size: u64::from(legacy.size),
                };
                Ok((window, window))
            }
        }
    }
}

impl IntelPlan {
    pub fn validate(&self) -> Result<fstart_core::FlashLayout, String> {
        self.reservations.validate()?;
        let (flash, firmware) = self.flash.windows()?;
        if flash != self.reservations.flash || firmware != self.reservations.firmware {
            return Err("physical layout differs from resolved flash/firmware windows".into());
        }
        if self.max_cpus == 0
            || self.smm.entry_points.is_some_and(|n| n < self.max_cpus)
            || self.smm.stack_size == 0
        {
            return Err("SMM entries/stacks must cover the MP CPU ceiling".into());
        }
        self.flash.decode()
    }
}
