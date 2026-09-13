//! Error types for Intel GMA display initialization.

/// Structured Intel GMA initialization error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GmaError {
    /// The expected PCI display function is absent.
    PciDeviceMissing,
    /// Required MMIO BAR/resource is unavailable.
    MmioUnavailable,
    /// Board policy requested VBT data, but none was supplied.
    VbtMissing,
    /// VBT/BDB data is malformed or unsupported.
    VbtInvalid,
    /// No usable display mode is available.
    ModeUnavailable,
    /// No PLL solution could be found for the selected mode.
    PllNoSolution,
    /// The requested output port is unsupported on this platform.
    UnsupportedPort,
    /// The requested CPU/platform path is not implemented yet.
    UnsupportedPlatform,
    /// Board display/scaler policy is internally inconsistent or unsafe.
    InvalidConfig,
    /// GTT setup or framebuffer mapping failed.
    GttSetupFailed,
    /// Hardware operation timed out.
    Timeout,
    /// Generic hardware error.
    HardwareError,
}

impl GmaError {
    /// Stable string identifier useful for firmware logs without formatting traits.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PciDeviceMissing => "PciDeviceMissing",
            Self::MmioUnavailable => "MmioUnavailable",
            Self::VbtMissing => "VbtMissing",
            Self::VbtInvalid => "VbtInvalid",
            Self::ModeUnavailable => "ModeUnavailable",
            Self::PllNoSolution => "PllNoSolution",
            Self::UnsupportedPort => "UnsupportedPort",
            Self::UnsupportedPlatform => "UnsupportedPlatform",
            Self::InvalidConfig => "InvalidConfig",
            Self::GttSetupFailed => "GttSetupFailed",
            Self::Timeout => "Timeout",
            Self::HardwareError => "HardwareError",
        }
    }
}
