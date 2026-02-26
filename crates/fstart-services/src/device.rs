//! Device trait — base lifecycle for all hardware devices.
//!
//! Every driver implements `Device` with an associated `Config` type that
//! captures exactly the resources it needs.  Codegen maps the RON driver
//! config to the driver-specific struct at build time.
//!
//! Bus-attached devices implement [`BusDevice`] instead, which adds
//! `new_on_bus` — codegen passes the parent bus controller reference
//! directly (compile-away approach: no runtime lookup).
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

/// Base trait for all root-level hardware devices.
///
/// Separates construction (`new`) from hardware initialisation (`init`) so
/// that codegen can control init ordering via the capability list.
///
/// # Associated Types
///
/// `Config` is the driver-specific configuration struct (e.g., `Ns16550Config`).
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

/// Trait for devices that live on a parent bus.
///
/// A bus device requires a parent controller that provides a bus-level
/// service (e.g., `I2cBus`, `SpiBus`).  Codegen resolves the parent at
/// build time and passes it to `new_on_bus` directly — no runtime device
/// lookup, no linked-list traversal.
///
/// # Example
///
/// ```ignore
/// impl<B: I2c> BusDevice for Slb9670<B> {
///     type Bus = B;
///     type Config = Slb9670Config;
///
///     fn new_on_bus(config: &Slb9670Config, bus: &B) -> Result<Self, DeviceError> {
///         Ok(Self { bus, addr: config.addr })
///     }
/// }
/// ```
///
/// Codegen generates:
/// ```ignore
/// let tpm0 = Slb9670::new_on_bus(&tpm0_config, &i2c0);
/// ```
pub trait BusDevice: Send + Sync + Sized {
    /// Human-readable driver name.
    const NAME: &'static str;

    /// Compatible strings.
    const COMPATIBLE: &'static [&'static str];

    /// Driver-specific configuration type (deserialized from RON).
    type Config;

    /// The parent bus interface type this device requires.
    ///
    /// Typically a concrete type or an embedded-hal trait bound
    /// (e.g., `B` where `B: I2c`).
    type Bus: ?Sized;

    /// Construct from config + parent bus reference.  Does NOT touch hardware.
    fn new_on_bus(config: &Self::Config, bus: &Self::Bus) -> Result<Self, DeviceError>;

    /// Initialise hardware.  Called after `new_on_bus()`, in capability order.
    fn init(&self) -> Result<(), DeviceError>;
}
