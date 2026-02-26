//! Device declaration types for board configuration.

use heapless::String as HString;
use serde::{Deserialize, Serialize};

/// A device declaration in the board configuration.
///
/// Carries the identity and service bindings for a hardware device.
/// The driver-specific configuration (register addresses, clocks, etc.)
/// lives in the [`fstart_drivers::DriverInstance`] enum — each driver
/// defines its own typed `Config` struct.  This separation means
/// `DeviceConfig` is purely metadata; the actual config shape is validated
/// by serde when the RON is parsed into `DriverInstance`.
///
/// Bus hierarchies are expressed via the `parent` field: a child device
/// sets `parent` to its bus controller's name.  Codegen ensures parents
/// are initialised before children.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceConfig {
    /// Device instance name (e.g., "uart0", "flash0").
    pub name: HString<32>,
    /// Driver name — derived from the `DriverInstance` variant
    /// (e.g., "ns16550", "pl011").  Used by xtask for feature derivation
    /// and by codegen for registry lookups.
    pub driver: HString<32>,
    /// Which service traits this device provides (e.g., ["Console"]).
    pub services: heapless::Vec<HString<32>, 8>,
    /// Parent device name (for bus-attached devices, e.g., "i2c0").
    /// `None` for root-level devices.
    #[serde(default)]
    pub parent: Option<HString<32>>,
}
