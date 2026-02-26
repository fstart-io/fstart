//! Driver registry — maps RON driver names to Rust type paths.
//!
//! This is the central lookup table that codegen uses to resolve a board's
//! `driver: "ns16550"` string to the actual `fstart_drivers::uart::ns16550::Ns16550`
//! type, its config struct, and the service traits it implements.

use fstart_types::DeviceConfig;

/// Information about a known driver — maps RON driver name to Rust type path
/// and its config construction logic.
pub(super) struct DriverInfo {
    /// RON driver name (e.g., "ns16550")
    pub name: &'static str,
    /// Rust module path (e.g., "fstart_drivers::uart::ns16550")
    pub module_path: &'static str,
    /// Rust type name (e.g., "Ns16550")
    pub type_name: &'static str,
    /// Rust config type name (e.g., "Ns16550Config")
    pub config_type: &'static str,
    /// Which service traits this driver implements.
    /// Used for flexible-mode enum dispatch codegen and validation.
    pub services: &'static [&'static str],
}

/// Registry of known drivers.
const KNOWN_DRIVERS: &[DriverInfo] = &[
    DriverInfo {
        name: "ns16550",
        module_path: "fstart_drivers::uart::ns16550",
        type_name: "Ns16550",
        config_type: "Ns16550Config",
        services: &["Console"],
    },
    DriverInfo {
        name: "pl011",
        module_path: "fstart_drivers::uart::pl011",
        type_name: "Pl011",
        config_type: "Pl011Config",
        services: &["Console"],
    },
    DriverInfo {
        name: "designware-i2c",
        module_path: "fstart_drivers::i2c::designware",
        type_name: "DesignwareI2c",
        config_type: "DesignwareI2cConfig",
        services: &["I2cBus"],
    },
];

/// Look up driver info by RON driver name.
pub(super) fn find_driver(name: &str) -> Option<&'static DriverInfo> {
    KNOWN_DRIVERS.iter().find(|d| d.name == name)
}

/// Bus service names that indicate a device is a bus controller.
const BUS_SERVICES: &[&str] = &["I2cBus", "SpiBus", "GpioController"];

/// Returns true if a device provides a bus service.
pub(super) fn is_bus_provider(dev: &DeviceConfig) -> bool {
    dev.services
        .iter()
        .any(|s| BUS_SERVICES.contains(&s.as_str()))
}
