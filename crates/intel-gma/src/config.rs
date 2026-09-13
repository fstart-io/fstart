//! Board display policy shared with chipset drivers.


use crate::types::Port;

/// Board policy for selecting a display mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreferredMode {
    /// Use `framebuffer.fallback_mode` as the fixed board-policy mode.
    Fixed,
    /// Use the VBT panel fixed mode, falling back to `framebuffer.fallback_mode`
    /// when no VBT panel mode is available.
    VbtPanel,
    /// Use EDID/DDC probing to select the first compatible candidate mode.
    Edid,
}

/// Per-output board enable policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputConfig {
    /// Logical output port.
    pub port: Port,
    /// Whether this output should be considered for init.
    pub enabled: bool,
}
