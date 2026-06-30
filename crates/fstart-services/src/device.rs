//! Device trait — base lifecycle for all hardware devices.
//!
//! Every driver implements `Device` with an associated `Config` type that
//! captures exactly the resources it needs. Rust board crates construct these
//! driver-specific structs directly.
//!
//! Bus-attached devices implement [`BusDevice`] instead. Board/platform-owned
//! topology builders pass the parent bus controller reference and typed bus address directly
//! (compile-away approach: no runtime lookup).
//!
//! See [docs/driver-model.md](../../../docs/driver-model.md) for the full design.

use fstart_types::BusAddress;

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
/// Separates construction (`new`) from hardware initialization. New static
/// board flows should prefer step-based [`crate::HardwareInit`] methods over
/// calling `init()` directly.
///
/// # Associated Types
///
/// `Config` is the driver-specific configuration struct (e.g., `Ns16550Config`).
/// Drivers own this value. Board code may build it on the stack, in a stage
/// device aggregate, or as a `static` constant for truly immutable facts, but
/// the service model does not require leaked or fake `'static` references.
pub trait Device: Send + Sync + Sized {
    /// Human-readable driver name (e.g., `"ns16550"`).
    const NAME: &'static str;

    /// Compatible strings for matching (e.g., `&["ns16550a", "ns16550"]`).
    const COMPATIBLE: &'static [&'static str];

    /// Driver-specific configuration type, with only the fields this driver needs.
    type Config;

    /// Construct from typed config. Does NOT touch hardware.
    fn new(config: Self::Config) -> Result<Self, DeviceError>;

    /// Legacy whole-device initialization hook. Prefer step-based
    /// [`crate::HardwareInit`] implementations for fixed-flow boards.
    fn init(&mut self) -> Result<(), DeviceError>;
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
///     fn new_on_bus(config: Slb9670Config, bus: &B) -> Result<Self, DeviceError> {
///         Self::new_on_bus_at(config, bus, None)
///     }
///
///     fn new_on_bus_at(
///         config: Slb9670Config,
///         bus: &B,
///         address: Option<BusAddress>,
///     ) -> Result<Self, DeviceError> {
///         let Some(BusAddress::I2c(addr)) = address else {
///             return Err(DeviceError::MissingResource("slb9670: missing I2C address"));
///         };
///         Ok(Self { bus, addr, config })
///     }
/// }
/// ```
///
/// Board-owned topology constructs children with typed bus addresses and then
/// initializes the child with mutable parent-bus access:
/// ```ignore
/// let mut tpm0 = Slb9670::new_on_bus_at(
///     tpm0_config,
///     &i2c0,
///     Some(BusAddress::I2c(0x50)),
/// );
/// tpm0.init_on_bus(&mut i2c0);
/// ```
pub trait BusDevice: Send + Sync + Sized {
    /// Human-readable driver name.
    const NAME: &'static str;

    /// Compatible strings.
    const COMPATIBLE: &'static [&'static str];

    /// Driver-specific configuration type supplied by board metadata.
    type Config;

    /// The parent bus interface type this device requires.
    ///
    /// Typically a concrete type or an embedded-hal trait bound
    /// (e.g., `B` where `B: I2c`).
    type Bus: ?Sized;

    /// Construct from config + parent bus reference. Does NOT touch hardware.
    fn new_on_bus(config: Self::Config, bus: &Self::Bus) -> Result<Self, DeviceError>;

    /// Construct from config + parent bus reference + bus address.
    ///
    /// Devices whose address is modeled by board topology must override this
    /// method and consume the supplied address. The default accepts only
    /// address-less topology, so board topology never silently drops bus wiring data.
    fn new_on_bus_at(
        config: Self::Config,
        bus: &Self::Bus,
        address: Option<BusAddress>,
    ) -> Result<Self, DeviceError> {
        if address.is_some() {
            return Err(DeviceError::MissingResource("bus_address_handler"));
        }
        Self::new_on_bus(config, bus)
    }

    /// Initialise hardware. Called after construction, in capability order.
    fn init(&mut self) -> Result<(), DeviceError>;

    /// Initialise hardware with access to the parent bus.
    fn init_on_bus(&mut self, bus: &mut Self::Bus) -> Result<(), DeviceError>;
}
