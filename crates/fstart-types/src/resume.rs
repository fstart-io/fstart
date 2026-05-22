//! S3 resume state shared by stages and chipset drivers.

use serde::{Deserialize, Serialize};

/// Firmware boot path selected by early chipset power-state detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum BootPath {
    /// Cold/reset boot; firmware owns hardware initialization and table generation.
    #[default]
    Normal,
    /// ACPI S3 resume; firmware must preserve OS memory and jump to the wake vector.
    S3Resume,
}

impl BootPath {
    /// Return true when this boot is an ACPI S3 resume.
    pub const fn is_s3_resume(self) -> bool {
        matches!(self, Self::S3Resume)
    }
}

/// Stage-cache backend selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StageCacheBackend {
    /// Cache in normal RAM and reserve the range from the OS memory map.
    Memory,
    /// Cache in TSEG/SMRAM. The chipset/SMM provider chooses the final base.
    Tseg,
}

/// Persistent compressed-stage cache configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StageCacheConfig {
    /// Cache backend.
    pub backend: StageCacheBackend,
    /// Base address for [`StageCacheBackend::Memory`]. Ignored for TSEG.
    #[serde(default)]
    pub base: u64,
    /// Cache capacity in bytes.
    pub size: u64,
}

/// Persistent stage-cache location used by the resume path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct StageCacheInfo {
    /// Physical base address of the cached compressed stage/FFS bytes.
    pub base: u64,
    /// Size in bytes of the cached region.
    pub size: u64,
}

/// Resume-specific inter-stage data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ResumeHandoff {
    /// Boot path detected by the earliest chipset stage.
    pub boot_path: BootPath,
    /// Whether persistent firmware memory/CBMEM-style state was recovered.
    pub persistent_memory_recovered: bool,
    /// Optional SMRAM/TSEG compressed stage cache.
    pub stage_cache: StageCacheInfo,
}
