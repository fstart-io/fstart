//! Device trait — base lifecycle for all hardware devices.
//!
//! Every driver implements `Device` with an associated `Config` type that
//! captures exactly the resources it needs.  Codegen maps the RON `Resources`
//! to the driver-specific config at build time.
//!
//! See [docs/driver-model.md](../../../docs/driver-model.md) for the full design.

/// Error type for device construction and initialisation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceError {
    /// A required resource was not provided in the board config.
    MissingResource(&'static str),
    /// The device configuration is invalid.
    ConfigError,
    /// Hardware did not respond as expected during init.
    InitFailed,
    /// A bus error occurred communicating with a parent bus.
    BusError,
}

/// Base trait for all hardware devices.
///
/// Separates construction (`new`) from hardware initialisation (`init`) so
/// that codegen can control init ordering via the capability list.
///
/// # Associated Types
///
/// `Config` is the driver-specific configuration struct (e.g., `Ns16550Config`).
/// It replaces the flat `Resources` grab-bag with per-driver typed fields.
pub trait Device: Send + Sync + Sized {
    /// Human-readable driver name (e.g., `"ns16550"`).
    const NAME: &'static str;

    /// Compatible strings for matching (e.g., `&["ns16550a", "ns16550"]`).
    const COMPATIBLE: &'static [&'static str];

    /// Driver-specific configuration type, with only the fields this driver needs.
    type Config;

    /// Construct from typed config.  Does NOT touch hardware.
    fn new(config: &Self::Config) -> Result<Self, DeviceError>;

    /// Initialise hardware.  Called after `new()`, in capability order.
    fn init(&self) -> Result<(), DeviceError>;
}
