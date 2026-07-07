//! Board-level System Management Mode (SMM) configuration.
//!
//! SMM handler images are built separately from normal stages. The selected
//! board crate supplies the handler binding; this data only controls image
//! layout and optional compatibility outputs.

use serde::{Deserialize, Serialize};

/// Optional coreboot compatibility outputs for the standalone SMM image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct CorebootSmmCompat {
    /// Generate a C header containing image-relative offsets that coreboot can
    /// include from its build. The header is an output artifact, not runtime
    /// data in the firmware image.
    #[serde(default)]
    pub emit_header: bool,
    /// Include the coreboot-style module-argument block in the image and emit
    /// its relative offset in the generated header.
    #[serde(default)]
    pub module_args: bool,
}

/// Top-level SMM image settings from board metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SmmConfig {
    /// Number of PIC entry stubs to precompile into the SMM image.
    ///
    /// If omitted, xtask uses the stage build's MP CPU count. When present,
    /// it must be greater than or equal to that count.
    #[serde(default)]
    pub entry_points: Option<u16>,
    /// Per-CPU SMM stack size in bytes.
    #[serde(default = "default_stack_size")]
    pub stack_size: u32,
    /// Coreboot compatibility outputs/ABI blocks.
    #[serde(default)]
    pub coreboot: CorebootSmmCompat,
}

impl Default for SmmConfig {
    fn default() -> Self {
        Self {
            entry_points: None,
            stack_size: default_stack_size(),
            coreboot: CorebootSmmCompat::default(),
        }
    }
}

const fn default_stack_size() -> u32 {
    0x400
}
