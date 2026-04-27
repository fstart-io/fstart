//! Board-level System Management Mode (SMM) configuration.
//!
//! SMM handler images are built separately from normal stages.  The board
//! configuration controls the platform-specific SMRAM adapter and the number
//! of precompiled PIC entry stubs the image must contain.

use serde::{Deserialize, Serialize};

/// SMM platform backend used to open/lock SMRAM and trigger relocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SmmPlatform {
    /// QEMU q35 / ICH9-compatible SMM model.
    QemuQ35,
    /// Intel Pineview northbridge with ICH7 southbridge.
    PineviewIch7,
}

/// Optional coreboot compatibility outputs for the standalone SMM image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CorebootSmmCompat {
    /// Generate a C header containing image-relative offsets that coreboot can
    /// include from its build.  The header is an output artifact, not runtime
    /// data in the firmware image.
    #[serde(default)]
    pub emit_header: bool,
    /// Include the coreboot-style module-argument block in the image and emit
    /// its relative offset in the generated header.
    #[serde(default)]
    pub module_args: bool,
}

/// Top-level SMM settings from `board.ron`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmmConfig {
    /// Platform-specific SMRAM/SMI backend.
    pub platform: SmmPlatform,
    /// Number of PIC entry stubs to precompile into the SMM image.
    ///
    /// If omitted, codegen/xtask should use the `MpInit.num_cpus` value of
    /// the stage that enables SMM.  When present, it must be greater than or
    /// equal to `MpInit.num_cpus`.
    #[serde(default)]
    pub entry_points: Option<u16>,
    /// Per-CPU SMM stack size in bytes.
    #[serde(default = "default_stack_size")]
    pub stack_size: u32,
    /// Coreboot compatibility outputs/ABI blocks.
    #[serde(default)]
    pub coreboot: CorebootSmmCompat,
}

const fn default_stack_size() -> u32 {
    0x400
}
