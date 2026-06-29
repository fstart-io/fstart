use crate::DriverInstance;
use heapless::String as HString;
use serde::{Deserialize, Serialize};

/// A Rust-board runtime driver bound to a named [`fstart_types::DeviceConfig`].
///
/// Migrated Rust board crates use named bindings so their device topology can
/// include driverless structural bus nodes without padding the driver list with
/// fake positional `Structural` entries. Codegen still lowers this into its
/// legacy parallel table internally.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DriverInstanceBinding {
    /// Device name in the board's flat topology table.
    pub device: HString<32>,
    /// Typed runtime driver configuration for that device.
    pub instance: DriverInstance,
}
impl DriverInstanceBinding {
    /// Bind a typed driver instance to a board device name.
    #[must_use]
    pub fn new(device: &str, instance: DriverInstance) -> Self {
        let mut name = HString::new();
        name.push_str(device)
            .expect("driver binding device name exceeds capacity");
        Self {
            device: name,
            instance,
        }
    }

    /// Cargo feature for this binding's driver, if it has one.
    #[must_use]
    pub fn driver_feature(&self) -> Option<&'static str> {
        self.instance.driver_feature()
    }
}
impl DriverInstance {
    /// Bind this instance to a named board device.
    #[must_use]
    pub fn bind(self, device: &str) -> DriverInstanceBinding {
        DriverInstanceBinding::new(device, self)
    }
}
